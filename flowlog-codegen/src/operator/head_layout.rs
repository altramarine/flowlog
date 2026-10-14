//! Reversible field orders shared by every source of a rule head.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HeadOutputLayout {
    columns: Vec<usize>,
}

impl HeadOutputLayout {
    /// Orders retained join keys, left values, then right values by input
    /// column index. Every projected column remains in the reversible row.
    /// Computed projections do not select a field order.
    fn from_projection(args: &[ArithmeticArgument]) -> Option<Self> {
        let mut ordered = Vec::with_capacity(args.len());
        for (column, arg) in args.iter().enumerate() {
            if !arg.rest().is_empty() {
                return None;
            }
            let FactorArgument::Var(TransformationArgument::Jn((left, key, index))) = arg.init()
            else {
                return None;
            };
            let group = if *key {
                0
            } else if *left {
                1
            } else {
                2
            };
            ordered.push((group, *index, column));
        }
        ordered.sort_by_key(|&(group, index, _)| (group, index));
        Some(Self {
            columns: ordered.into_iter().map(|(_, _, column)| column).collect(),
        })
    }

    pub(super) fn reorder_row(&self) -> TokenStream {
        let columns = self.columns.iter().copied().map(Index::from);
        quote! { (#(row.#columns,)*) }
    }

    pub(super) fn restore_row(&self) -> TokenStream {
        let mut columns = vec![Index::from(0); self.columns.len()];
        for (physical, &logical) in self.columns.iter().enumerate() {
            columns[logical] = Index::from(physical);
        }
        quote! { (#(row.#columns,)*) }
    }
}

/// Every contribution to a relation uses one field order. Shared join
/// results keep their logical layout until their rule-head contribution.
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
            .flat_map(|step| step.input_fingerprints())
            .chain(
                plan.strata()
                    .iter()
                    .flat_map(|stratum| stratum.recursion_enter_collections().iter().copied()),
            )
            .collect();
        let mut layouts = Self::default();
        for stratum in plan.strata() {
            for (&relation, heads) in stratum.idb_to_heads_map() {
                let layout = transformations
                    .iter()
                    .copied()
                    .filter(|step| heads.contains(&step.output().fingerprint()))
                    .filter_map(|step| {
                        if let Transformation::JnToRow { flow, .. } = step {
                            let layout = HeadOutputLayout::from_projection(flow.value())?;
                            let left = flow.value().iter().any(|arg| {
                                matches!(
                                    arg.init(),
                                    FactorArgument::Var(TransformationArgument::Jn((
                                        true,
                                        false,
                                        _
                                    )))
                                )
                            });
                            let right = flow.value().iter().any(|arg| {
                                matches!(
                                    arg.init(),
                                    FactorArgument::Var(TransformationArgument::Jn((
                                        false,
                                        false,
                                        _
                                    )))
                                )
                            });
                            Some((!(left && right), layout))
                        } else {
                            None
                        }
                    })
                    // Two-sided projections expose join enumeration order.
                    // Prefer them to one-sided seeds; without cardinalities,
                    // this need not select the largest source.
                    .min_by_key(|(one_sided, _)| *one_sided)
                    .map(|(_, layout)| layout);
                if let Some(layout) = layout {
                    // All strata must agree before any join changes its row.
                    layouts.relations.insert(relation, layout);
                }
            }
        }
        layouts
            .relations
            .retain(|_, layout| layout.columns.iter().copied().ne(0..layout.columns.len()));
        let mut conflicts = HashSet::new();
        for stratum in plan.strata() {
            for (&relation, heads) in stratum.idb_to_heads_map() {
                let Some(layout) = layouts.relations.get(&relation) else {
                    // A shared result must also serve heads that keep logical
                    // column order, so those consumers prevent direct rewrites.
                    for fingerprint in heads {
                        layouts.joins.remove(fingerprint);
                        conflicts.insert(*fingerprint);
                    }
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
                            .is_some_and(|old| old != layout)
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

    pub(super) fn join_projection(
        &self,
        fingerprint: u64,
        projection: &[ArithmeticArgument],
    ) -> Option<Vec<ArithmeticArgument>> {
        let layout = self.joins.get(&fingerprint)?;
        Some(
            layout
                .columns
                .iter()
                .map(|&column| projection[column].clone())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use flowlog_parser::ArithmeticOperator;

    use super::*;
    use crate::test_harness::codegen;

    fn column(left: bool, key: bool, index: usize) -> ArithmeticArgument {
        ArithmeticArgument {
            init: FactorArgument::Var(TransformationArgument::Jn((left, key, index))),
            rest: Vec::new(),
        }
    }

    /// The physical field-order policy is private to code generation.
    #[test]
    fn retained_join_keys_precede_input_ordered_left_and_right_values() {
        let layout = HeadOutputLayout::from_projection(&[
            column(false, false, 2),
            column(true, false, 1),
            column(false, true, 2),
            column(false, false, 0),
            column(true, false, 0),
            column(true, true, 0),
            column(true, false, 2),
            column(false, false, 1),
            column(true, true, 1),
        ])
        .unwrap();
        assert_eq!(layout.columns, vec![5, 8, 2, 4, 1, 6, 3, 7, 0]);
        assert_eq!(
            layout.restore_row().to_string(),
            quote! { (row.8, row.4, row.2, row.6, row.3, row.0, row.5, row.7, row.1,) }.to_string(),
        );
    }

    #[test]
    fn dropped_input_columns_are_not_added_to_the_dedup_row() {
        let layout = HeadOutputLayout::from_projection(&[
            column(false, false, 3),
            column(true, false, 2),
            column(false, true, 1),
        ])
        .unwrap();
        assert_eq!(layout.columns, vec![2, 1, 0]);
    }

    #[test]
    fn repeated_projected_columns_remain_reversible() {
        let layout = HeadOutputLayout::from_projection(&[
            column(false, false, 2),
            column(true, false, 1),
            column(false, false, 0),
            column(true, false, 1),
        ])
        .unwrap();
        assert_eq!(layout.columns, vec![1, 3, 2, 0]);
        assert_eq!(
            layout.restore_row().to_string(),
            quote! { (row.3, row.0, row.2, row.1,) }.to_string(),
        );
    }

    #[test]
    fn computed_projections_keep_their_logical_layout() {
        let mut expression = column(true, false, 0);
        expression
            .rest
            .push((ArithmeticOperator::Plus, column(false, false, 0).init));
        assert!(HeadOutputLayout::from_projection(&[expression]).is_none());
    }

    #[test]
    fn one_sided_seeds_do_not_select_the_recursive_join_field_order() {
        let codegen = codegen(
            ".decl Seed(token: int32, heap: int32, var: int32)\n\
             .input Seed\n\
             .decl Token(token: int32)\n\
             .input Token\n\
             .decl Assign(src: int32, dst: int32)\n\
             .input Assign\n\
             .decl VarPointsTo(heap: int32, var: int32)\n\
             VarPointsTo(h,v) :- Token(t), Seed(t,h,v).\n\
             VarPointsTo(h,to) :- Assign(from,to), VarPointsTo(h,from).\n\
             .output VarPointsTo\n",
        );
        let plan = ProgramPlanner::from_program(&codegen.program, &mut None).unwrap();
        let layouts = HeadOutputLayouts::from_plan(&plan);
        let relation = codegen.program.idbs()[0].fingerprint();
        assert_eq!(layouts.relations[&relation].columns, vec![1, 0]);
    }
}
