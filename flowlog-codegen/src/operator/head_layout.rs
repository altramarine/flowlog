//! Shared physical layouts for all sources contributing to a rule head.

use std::collections::HashMap;
use std::collections::HashSet;

use flowlog_planner::planner::ArithmeticArgument;
use flowlog_planner::planner::FactorArgument;
use flowlog_planner::planner::ProgramPlanner;
use flowlog_planner::planner::Transformation;
use flowlog_planner::planner::TransformationArgument;
use proc_macro2::TokenStream;
use quote::quote;
use syn::Index;

use super::join_layout::JoinOutputLayout;
use crate::Codegen;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct HeadOutputLayout {
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

    pub(super) fn split_row(&self) -> TokenStream {
        let keys: Vec<_> = self.key_columns.iter().copied().map(Index::from).collect();
        let values: Vec<_> = self
            .value_columns
            .iter()
            .copied()
            .map(Index::from)
            .collect();
        quote! { ((#(row.#keys,)*), (#(row.#values,)*)) }
    }

    pub(super) fn restore_row(&self) -> TokenStream {
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
pub(crate) struct HeadOutputLayouts {
    pub(super) relations: HashMap<u64, HeadOutputLayout>,
    pub(super) joins: HashMap<u64, HeadOutputLayout>,
}

impl HeadOutputLayouts {
    pub(crate) fn from_plan(plan: &ProgramPlanner) -> Self {
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
                        let arity = step.output().arity().1;
                        // Without a retained join-side group, preserve column
                        // order and split it in half to shorten the trace key.
                        // Complete pairs still determine worker placement.
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

impl Codegen {
    pub(crate) fn grouped_head_join_projection(
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
}
