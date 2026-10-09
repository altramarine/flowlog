//! Shared physical layouts for all sources contributing to a rule head.

use std::collections::HashMap;
use std::collections::HashSet;

use flowlog_planner::planner::ArithmeticArgument;
use flowlog_planner::planner::FactorArgument;
use flowlog_planner::planner::ProgramPlanner;
use flowlog_planner::planner::Transformation;
use flowlog_planner::planner::TransformationArgument;
use flowlog_profiler::PlanGraph;
use flowlog_profiler::with_plan_graph;
use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::quote;
use syn::Index;

use super::join_layout::JoinOutputLayout;
use crate::codegen::CodeGen;
use crate::codegen::CodegenError;

#[derive(Clone, PartialEq, Eq)]
struct HeadOutputLayout {
    key_columns: Vec<usize>,
    value_columns: Vec<usize>,
}

impl HeadOutputLayout {
    fn from_join(layout: JoinOutputLayout) -> Self {
        let mut key_columns = Vec::with_capacity(layout.key.len());
        let mut value_columns = Vec::with_capacity(layout.value.len());
        for (column, argument) in layout.row.iter().enumerate() {
            if matches!(
                argument.init(),
                FactorArgument::Var(TransformationArgument::KV((true, _)))
            ) {
                key_columns.push(column);
            } else {
                value_columns.push(column);
            }
        }
        Self {
            key_columns,
            value_columns,
        }
    }

    fn split_row(&self) -> TokenStream {
        let keys: Vec<_> = self.key_columns.iter().copied().map(Index::from).collect();
        let values: Vec<_> = self
            .value_columns
            .iter()
            .copied()
            .map(Index::from)
            .collect();
        quote! { ((#(row.#keys,)*), (#(row.#values,)*)) }
    }

    fn restore_row(&self) -> TokenStream {
        let mut fields =
            vec![TokenStream::new(); self.key_columns.len() + self.value_columns.len()];
        for (index, &column) in self.key_columns.iter().enumerate() {
            let index = Index::from(index);
            fields[column] = quote! { k.#index };
        }
        for (index, &column) in self.value_columns.iter().enumerate() {
            let index = Index::from(index);
            fields[column] = quote! { v.#index };
        }
        quote! { (#(#fields,)*) }
    }
}

/// One pair layout per relation; direct join heads use that same layout.
/// Shared intermediate rows retain their logical shape for other consumers.
#[derive(Default)]
pub(in crate::codegen) struct HeadOutputLayouts {
    relations: HashMap<u64, HeadOutputLayout>,
    joins: HashMap<u64, HeadOutputLayout>,
}

impl HeadOutputLayouts {
    pub(in crate::codegen) fn from_plan(plan: &ProgramPlanner) -> Self {
        let transformations: Vec<_> = plan
            .strata()
            .iter()
            .flat_map(|stratum| {
                stratum
                    .non_recursive_transformations()
                    .iter()
                    .chain(stratum.recursive_transformations())
            })
            .collect();
        let consumed: HashSet<_> = transformations
            .iter()
            .flat_map(|transformation| transformation.input_fingerprints())
            .chain(
                plan.strata()
                    .iter()
                    .flat_map(|stratum| stratum.recursion_enter_collections().iter().copied()),
            )
            .collect();
        let mut layouts = Self::default();
        for stratum in plan.strata() {
            for (&relation, heads) in stratum.idb_to_heads_map() {
                let head_steps: Vec<_> = transformations
                    .iter()
                    .copied()
                    .filter(|step| heads.contains(&step.output().fingerprint()))
                    .collect();
                let layout = head_steps
                    .iter()
                    .find_map(|step| {
                        if let Transformation::JnToRow { input, flow, .. } = step
                            && input.0.arity().0 > 0
                        {
                            JoinOutputLayout::from_projection(flow.value())
                                .map(HeadOutputLayout::from_join)
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        let step = head_steps.first()?;
                        let arity = step.flow().value().len();
                        // Without a retained join-side group, preserve column
                        // order and split it in half. This still groups values
                        // under a shorter key, but its partition can be skewed.
                        (arity >= 2).then(|| HeadOutputLayout {
                            key_columns: (0..arity / 2).collect(),
                            value_columns: (arity / 2..arity).collect(),
                        })
                    });
                let Some(layout) = layout else {
                    continue;
                };
                // A relation can receive contributions in several strata.
                // Select its final layout before changing any join output.
                layouts.relations.insert(relation, layout);
            }
        }
        let mut conflicts = HashSet::new();
        for stratum in plan.strata() {
            for (&relation, heads) in stratum.idb_to_heads_map() {
                let Some(layout) = layouts.relations.get(&relation) else {
                    continue;
                };
                for step in transformations
                    .iter()
                    .copied()
                    .filter(|step| heads.contains(&step.output().fingerprint()))
                {
                    let fingerprint = step.output().fingerprint();
                    if matches!(step, Transformation::JnToRow { .. })
                        && !consumed.contains(&fingerprint)
                        && !conflicts.contains(&fingerprint)
                    {
                        if layouts
                            .joins
                            .get(&fingerprint)
                            .is_some_and(|previous| previous != layout)
                        {
                            layouts.joins.remove(&fingerprint);
                            conflicts.insert(fingerprint);
                        } else {
                            layouts.joins.insert(fingerprint, layout.clone());
                        }
                    }
                }
            }
        }
        layouts
    }
}

impl CodeGen {
    pub(super) fn grouped_head_join_projection(
        &self,
        fingerprint: u64,
        projection: &[ArithmeticArgument],
    ) -> Option<(Vec<ArithmeticArgument>, Vec<ArithmeticArgument>)> {
        let layout = self.head_output_layouts.joins.get(&fingerprint)?;
        Some((
            layout
                .key_columns
                .iter()
                .map(|&column| projection[column].clone())
                .collect(),
            layout
                .value_columns
                .iter()
                .map(|&column| projection[column].clone())
                .collect(),
        ))
    }

    /// Merges every source before normalizing relation membership. Pair
    /// grouping remains intact through normalization and is restored once.
    pub(super) fn gen_head_dedup(
        &self,
        relation: u64,
        sources: &[(u64, Ident)],
        output: &Ident,
        recursive: bool,
        plan_graph: &mut Option<PlanGraph>,
    ) -> Result<TokenStream, CodegenError> {
        let layout = self.head_output_layouts.relations.get(&relation);
        let mut mapped = 0;
        let expressions: Vec<_> = sources
            .iter()
            .map(|(fingerprint, source)| {
                if let Some(layout) = layout {
                    if self.head_output_layouts.joins.get(fingerprint) == Some(layout) {
                        quote! { #source.clone() }
                    } else {
                        mapped += 1;
                        let split = layout.split_row();
                        quote! { #source.clone().map(|row| #split) }
                    }
                } else {
                    quote! { #source.clone() }
                }
            })
            .collect();
        let (head, tail) = expressions.split_first().ok_or_else(|| {
            CodegenError::internal(format!(
                "relation 0x{relation:016x} has no rule-head source"
            ))
        })?;
        let concatenate = u32::from(!tail.is_empty());
        let union = if tail.is_empty() {
            quote! { #head }
        } else {
            quote! { (#head).concatenate([#(#tail),*]) }
        };
        let inputs = sources
            .iter()
            .map(|(_, source)| source.to_string())
            .collect();
        if let Some(layout) = layout {
            let row = layout.restore_row();
            with_plan_graph(plan_graph, |graph| {
                graph.concat_keyed_dedup_operator(
                    self.display_name(relation),
                    inputs,
                    output.to_string(),
                    mapped + concatenate,
                    recursive,
                );
            });
            Ok(quote! {
                let #output = ::flowlog_runtime::operators::flowlog_dedup_by_key(#union)
                    .map(|(k, v)| #row);
            })
        } else {
            with_plan_graph(plan_graph, |graph| {
                graph.concat_dedup_operator(
                    self.display_name(relation),
                    inputs,
                    output.to_string(),
                    concatenate,
                    recursive,
                );
            });
            Ok(quote! {
                let #output = ::flowlog_runtime::operators::flowlog_dedup(#union);
            })
        }
    }
}
