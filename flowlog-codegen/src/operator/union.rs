//! Unions: the step a stratum chains to bind a relation from its parts,
//! the collections its rules produce for it and its earlier binding, each
//! at the weight the stratum gives it. [`Codegen::gen_union`] unions them
//! into one expression, applying the planned head operation, and
//! records the step in the plan graph under the name its caller binds;
//! the strata own the parts, the names and the order.
//!
//! Differential Dataflow's `concatenate` needs every part at one weight,
//! so a relation unions at its most mutable part's weight, and each part
//! below that weight is lifted to it first.

use flowlog_parser::Mutability;
use flowlog_planner::planner::HeadOperation;
use flowlog_profiler::PlanGraph;
use flowlog_profiler::with_plan_graph;
use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;

use crate::Codegen;
use crate::CodegenError;
use crate::tuple_tokens;
use crate::ty::diff::weight_tokens;

impl Codegen {
    /// Returns the union of `parts` at their most mutable weight, with
    /// the planner's `operation`: a whole-row dedup in the selected order,
    /// or unchanged rows for the declared aggregation that follows.
    /// Records the runtime steps under `output`, inside a loop when
    /// `recursive` is true. Reports an internal error for no inputs or
    /// an invalid dedup permutation.
    pub(crate) fn gen_union(
        &self,
        idb_fp: u64,
        parts: &[(Ident, Mutability)],
        operation: &HeadOperation,
        output: &Ident,
        recursive: bool,
        plan_graph: &mut Option<PlanGraph>,
    ) -> Result<(TokenStream, Mutability), CodegenError> {
        let name = self.display_name(idb_fp);
        let Some(((first, first_mutability), rest)) = parts.split_first() else {
            return Err(CodegenError::internal(format!(
                "relation `{name}` has no collections to union"
            )));
        };
        let (rows_order, dedup) = match operation {
            HeadOperation::Dedup { order } => (Some(order.as_slice()), true),
            HeadOperation::Aggregation => (None, false),
        };
        let mutability = rest
            .iter()
            .map(|(_, part)| *part)
            .fold(*first_mutability, Mutability::max);
        // The lift names its target weight: an arrangement of the union
        // dispatches on the weight before the concatenation would fix it.
        let lift_name = format!("Lift: {name}");
        let weight = weight_tokens(mutability);
        let lift = |collection: &Ident, part: Mutability| {
            if part < mutability {
                quote! {
                    ::flowlog_runtime::operators::flowlog_lift::<#weight, _, _, _>(
                        #collection.clone(), #lift_name,
                    )
                }
            } else {
                quote! { #collection.clone() }
            }
        };
        let head = lift(first, *first_mutability);
        let tail: Vec<TokenStream> = rest.iter().map(|(c, part)| lift(c, *part)).collect();
        let union = if tail.is_empty() {
            head
        } else {
            quote! { #head.concatenate([ #( #tail ),* ]) }
        };

        let (rows, layout_maps) = match rows_order {
            Some(order) => self.gen_head_dedup(idb_fp, union, order)?,
            None => (union, 0),
        };

        with_plan_graph(plan_graph, |plan_graph| {
            // A part less mutable than the relation is lifted.
            let lifts: u32 = parts
                .iter()
                .map(|(_, part)| u32::from(*part < mutability))
                .sum();
            plan_graph.union_operator(
                name.to_string(),
                parts.iter().map(|(id, _)| id.to_string()).collect(),
                output.to_string(),
                lifts + layout_maps,
                u32::from(!tail.is_empty()),
                dedup.then_some((mutability, recursive)),
            );
        });

        Ok((rows, mutability))
    }

    /// Applies the planned whole-row comparator while keeping the logical
    /// output schema. Returns the generated maps' count for profiling.
    fn gen_head_dedup(
        &self,
        relation: u64,
        rows: TokenStream,
        order: &[usize],
    ) -> Result<(TokenStream, u32), CodegenError> {
        let columns = self.find_global_type(relation)?.1.len();
        let mut sorted = order.to_vec();
        sorted.sort_unstable();
        if !sorted.iter().copied().eq(0..columns) {
            return Err(CodegenError::internal(format!(
                "head dedup of `{}` has order {order:?}, expected a permutation of {columns} columns",
                self.display_name(relation),
            )));
        }
        if order.iter().copied().eq(0..columns) {
            return Ok((
                quote! { ::flowlog_runtime::operators::flowlog_dedup(#rows) },
                0,
            ));
        }
        let fields: Vec<Ident> = (0..columns)
            .map(|column| format_ident!("x{column}"))
            .collect();
        let logical = tuple_tokens(fields.iter().map(|field| quote! { #field }));
        let physical = tuple_tokens(order.iter().map(|&column| {
            let field = &fields[column];
            quote! { #field }
        }));
        let name = self.display_name(relation);
        let reorder_name = format!("DedupOrder: {name}");
        let restore_name = format!("DedupRestore: {name}");
        Ok((
            quote! {
                ::flowlog_runtime::operators::flowlog_map(
                    ::flowlog_runtime::operators::flowlog_dedup(
                        ::flowlog_runtime::operators::flowlog_map(
                            #rows, #reorder_name,
                            |#logical, t, d| std::iter::once((#physical, t, d)),
                        )
                    ),
                    #restore_name,
                    |#physical, t, d| std::iter::once((#logical, t, d)),
                )
            },
            2,
        ))
    }
}

#[cfg(test)]
mod tests {
    use proc_macro2::Span;
    use rstest::rstest;

    use super::*;
    use crate::test_harness::codegen;

    /// A static input `R` and a static output `S` derived from it.
    const PROGRAM: &str =
        ".decl R(a: int32)\n.input R\n.decl S(a: int32)\nS(a) :- R(a).\n.output S\n";

    /// As [`PROGRAM`], with `S` a lattice relation.
    const LATTICE_PROGRAM: &str = ".decl R(a: int32, b: int32)\n.input R\n\
        .decl S(a: int32, b: min<int32>)\nS(a, b) :- R(a, b).\n.output S\n";

    fn ident(name: &str) -> Ident {
        Ident::new(name, Span::call_site())
    }

    /// `S` unions its parts in order at its most mutable part's weight,
    /// lifting only the parts below it.
    // Cases: parts with their weight, expected, weight.
    #[rstest]
    #[case::one_static_part(
        vec![("t_1", Mutability::Static)],
        quote! { ::flowlog_runtime::operators::flowlog_dedup(t_1.clone()) },
        Mutability::Static
    )]
    #[case::static_parts(
        vec![("t_1", Mutability::Static), ("t_2", Mutability::Static)],
        quote! { ::flowlog_runtime::operators::flowlog_dedup(t_1.clone().concatenate([t_2.clone()])) },
        Mutability::Static
    )]
    #[case::mixed_parts(
        vec![("t_1", Mutability::Static), ("t_2", Mutability::Mutable)],
        quote! {
            ::flowlog_runtime::operators::flowlog_dedup(
                ::flowlog_runtime::operators::flowlog_lift::<::flowlog_runtime::diff::Mutable, _, _, _>(
                    t_1.clone(), "Lift: S",
                )
                .concatenate([t_2.clone()])
            )
        },
        Mutability::Mutable
    )]
    #[case::earlier_binding_first(
        vec![("earlier", Mutability::Static), ("t_1", Mutability::Static)],
        quote! { ::flowlog_runtime::operators::flowlog_dedup(earlier.clone().concatenate([t_1.clone()])) },
        Mutability::Static
    )]
    fn a_relation_unions_its_parts_at_the_most_mutable_weight(
        #[case] parts: Vec<(&str, Mutability)>,
        #[case] expected: TokenStream,
        #[case] mutability: Mutability,
    ) {
        let codegen = codegen(PROGRAM);
        let s = codegen.program.idbs()[0].fingerprint();
        let parts: Vec<(Ident, Mutability)> =
            parts.into_iter().map(|(p, m)| (ident(p), m)).collect();
        let (deduped, found) = codegen
            .gen_union(
                s,
                &parts,
                &HeadOperation::Dedup { order: vec![0] },
                &ident("r"),
                false,
                &mut None,
            )
            .expect("parts to union");
        assert_eq!(
            (deduped.to_string(), found),
            (expected.to_string(), mutability)
        );
    }

    #[test]
    fn a_planned_dedup_order_is_restored_after_deduplication() {
        let codegen = codegen(
            ".decl R(a: int32, b: symbol, c: int64)\n.input R\n\
             .decl S(a: int32, b: symbol, c: int64)\nS(a,b,c) :- R(a,b,c).\n.output S\n",
        );
        let relation = codegen.program.idbs()[0].fingerprint();
        let (rows, maps) = codegen
            .gen_head_dedup(relation, quote! { rows }, &[2, 0, 1])
            .expect("a permutation of all three columns");
        let expected = quote! {
            ::flowlog_runtime::operators::flowlog_map(
                ::flowlog_runtime::operators::flowlog_dedup(
                    ::flowlog_runtime::operators::flowlog_map(
                        rows, "DedupOrder: S",
                        |(x0,x1,x2), t, d| std::iter::once(((x2,x0,x1), t, d)),
                    )
                ),
                "DedupRestore: S",
                |(x2,x0,x1), t, d| std::iter::once(((x0,x1,x2), t, d)),
            )
        };
        syn::parse2::<syn::Expr>(rows.clone()).expect("a valid Rust expression");
        assert_eq!((rows.to_string(), maps), (expected.to_string(), 2));
    }

    // Cases: invalid whole-row permutations.
    #[rstest]
    #[case(vec![])]
    #[case(vec![0, 0])]
    #[case(vec![1])]
    fn an_invalid_dedup_permutation_is_an_internal_error(#[case] order: Vec<usize>) {
        let codegen = codegen(PROGRAM);
        let relation = codegen.program.idbs()[0].fingerprint();
        let error = codegen
            .gen_head_dedup(relation, quote! { rows }, &order)
            .expect_err("the one column must appear exactly once");
        assert!(matches!(error, CodegenError::Internal(_)));
    }

    /// A lattice relation's union is not dedupped: its merge makes the set.
    #[test]
    fn a_lattice_relations_union_is_not_dedupped() {
        let codegen = codegen(LATTICE_PROGRAM);
        let s = codegen.program.idbs()[0].fingerprint();
        let parts = [
            (ident("t_1"), Mutability::Static),
            (ident("t_2"), Mutability::Static),
        ];
        let (rows, found) = codegen
            .gen_union(
                s,
                &parts,
                &HeadOperation::Aggregation,
                &ident("r"),
                false,
                &mut None,
            )
            .expect("parts to union");
        assert_eq!(
            (rows.to_string(), found),
            (
                quote! { t_1.clone().concatenate([t_2.clone()]) }.to_string(),
                Mutability::Static
            )
        );
    }

    /// A relation with no part has nothing to union: a planner bug, not an
    /// empty collection.
    #[test]
    fn a_relation_with_no_part_is_an_internal_error() {
        let codegen = codegen(PROGRAM);
        let s = codegen.program.idbs()[0].fingerprint();
        let error = codegen
            .gen_union(
                s,
                &[],
                &HeadOperation::Dedup { order: vec![0] },
                &ident("r"),
                false,
                &mut None,
            )
            .expect_err("nothing to union");
        assert!(matches!(error, CodegenError::Internal(_)));
    }
}
