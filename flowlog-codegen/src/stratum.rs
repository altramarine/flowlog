//! Strata in evaluation order, each generated whole by [`non_recursive`]
//! or [`recursive`]: its prelude, the planned rule bodies that run outside
//! any loop, then its heads or its loop. The rules' pieces come from
//! [`rule`](crate::rule); a relation binds the union of its parts, merged
//! by its lattice column when it has one.

mod non_recursive;
mod recursive;

use std::mem;

use flowlog_planner::planner::StratumPlanner;
use flowlog_profiler::PlanGraph;
use flowlog_profiler::with_plan_graph;
use proc_macro2::TokenStream;
use quote::quote;
use tracing::trace;

use crate::Codegen;
use crate::CodegenError;

impl Codegen {
    /// Returns every stratum of the plan, one fragment each, in evaluation
    /// order.
    pub(crate) fn gen_strata(
        &mut self,
        strata: &[StratumPlanner],
        plan_graph: &mut Option<PlanGraph>,
    ) -> Result<Vec<TokenStream>, CodegenError> {
        let mut flows = Vec::with_capacity(strata.len());
        let evaluation_order = strata.iter().enumerate();
        for (idx, stratum) in evaluation_order {
            with_plan_graph(plan_graph, |plan_graph| {
                plan_graph.update_stratum_block(idx);
            });
            flows.push(if stratum.is_recursive() {
                self.gen_recursive(stratum, plan_graph)?
            } else {
                self.gen_non_recursive(stratum, plan_graph)?
            });
        }
        Ok(flows)
    }

    /// Returns the stratum's prelude: its planned steps outside any loop,
    /// into the program-wide outer-scope arrangement cache
    /// (`self.outer_fp_to_arrangement`). A recursive stratum has one too when
    /// the planner factors work that reads no feedback out of its loop.
    fn gen_prelude(
        &mut self,
        stratum: &StratumPlanner,
        plan_graph: &mut Option<PlanGraph>,
    ) -> Result<Vec<TokenStream>, CodegenError> {
        let global_fp_to_ident = self.global_fp_to_ident.clone();
        let mut outer_fp_to_arrangement = mem::take(&mut self.outer_fp_to_arrangement);
        let flows = stratum
            .non_recursive_transformations()
            .iter()
            .map(|transformation| {
                self.gen_operator(
                    &global_fp_to_ident,
                    transformation,
                    &mut outer_fp_to_arrangement,
                    stratum,
                    plan_graph,
                )
            })
            .collect::<Result<Vec<_>, _>>();
        self.outer_fp_to_arrangement = outer_fp_to_arrangement;
        let flows = flows?;
        trace!("Generated prelude:\n{}\n", quote! { #(#flows)* });
        Ok(flows)
    }
}
