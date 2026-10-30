//! A non-recursive stratum: its prelude, then each relation's head step,
//! the union of its rule heads with its earlier binding, merged by its
//! lattice column when it has one, outside any loop.

use flowlog_parser::Mutability;
use flowlog_planner::planner::StratumPlanner;
use flowlog_profiler::PlanGraph;
use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::quote;

use crate::Codegen;
use crate::CodegenError;

impl Codegen {
    /// Returns a non-recursive `stratum`: its prelude, then the head step
    /// of every relation it derives, each rebinding the relation's global
    /// binding. The plan selects its earlier seed and head operation;
    /// each part keeps its recorded weight until the union lifts it.
    pub(super) fn gen_non_recursive(
        &mut self,
        stratum: &StratumPlanner,
        plan_graph: &mut Option<PlanGraph>,
    ) -> Result<TokenStream, CodegenError> {
        let prelude = self.gen_prelude(stratum, plan_graph)?;
        let mut heads = Vec::new();
        for (idb_fp, head_plan) in stratum.heads() {
            let name = self.display_name(*idb_fp);
            let binding = self.find_global_ident(*idb_fp);
            // Fold into the existing binding rather than shadowing it. A
            // relation whose earlier binding holds a static input, or a
            // static partial result from an earlier stratum, keeps that
            // binding's weight until the union lifts it.
            let earlier = head_plan.seed();
            // Each part, the earlier binding then the heads, at the weight
            // recorded for it: a head is a step's output, or the relation a
            // rule reads as it is, bound globally outside a loop.
            let parts: Vec<(Ident, Mutability)> = earlier
                .into_iter()
                .map(|fp| Ok((self.find_global_ident(fp), self.mutability(fp)?)))
                .chain(
                    head_plan
                        .inputs()
                        .iter()
                        .map(|fp| Ok((self.find_global_ident(*fp), self.mutability(*fp)?))),
                )
                .collect::<Result<_, CodegenError>>()?;
            let (head, mutability) = self.gen_head(
                *idb_fp,
                &parts,
                head_plan.operation(),
                &binding,
                false,
                plan_graph,
            )?;
            // The stratifier assigns the relation the same value on its own;
            // the two derivations agree by Lemma 4 of
            // `docs/design/mutability.md`.
            debug_assert_eq!(
                stratum.mutability(*idb_fp),
                Some(mutability),
                "`{name}` binds at a weight the stratifier does not give it",
            );
            self.global_fp_to_mutability.insert(*idb_fp, mutability);
            heads.push(quote! { let #binding = #head; });
        }
        Ok(quote! { #(#prelude)* #(#heads)* })
    }
}
