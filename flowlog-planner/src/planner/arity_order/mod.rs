//! Attribute-ordering stages over the existing physical plan.

mod graph;
mod initialize;

use flowlog_parser::Program;
use initialize::initialize;
use tracing::trace;

use super::StratumPlanner;

/// Initializes physical dependencies without changing the plan.
/// `upstream` contains earlier strata in evaluation order; `downstream`
/// contains already processed strata, from last to nearest.
/// Layout domains, costs, solving, and rewriting remain unimplemented.
#[must_use]
pub(super) fn optimize(
    stratum: StratumPlanner,
    upstream: &[StratumPlanner],
    downstream: &[StratumPlanner],
    program: &Program,
) -> StratumPlanner {
    // Step 1: identify arrangements, fuse projection mappings within
    // each feed, and retain union and recursive dependencies.
    let graph = initialize(&stratum, upstream, downstream, program);
    for edge in &graph.edges {
        trace!(
            input = ?edge.input,
            output = ?edge.output,
            "initialized arrangement provenance",
        );
    }
    trace!(
        arrangements = graph.nodes.len(),
        edges = graph.edges.len(),
        "initialized attribute-ordering physical graph",
    );

    // TODO: Step 2: build legal layout domains and costs from the fused
    // feeds. Couple each join's input key orders and orientation to its
    // output; all union feeds share the target's layout. Extract the
    // sorting boundaries of aggregation operators and relation bindings
    // before solving.
    // Resolve external schemas before solving.
    // Keep downstream choices fixed and include their conversion costs;
    // an enter or feedback edge does not create an arrangement.

    // TODO: Step 3: eliminate fixed choices and contract only blocks whose
    // consecutive placement is proved to preserve an optimum.

    // TODO: Step 4: select a small-product cut of the undirected factor
    // graph and enumerate its assignments, leaving a forest each time.

    // TODO: Step 5: solve each forest with messages in both directions,
    // considering legal join orientations and preferring main on ties.

    // TODO: Step 6: apply the chosen field permutations and orientations
    // to the stratum, updating every dependent reference consistently.

    // TODO: Step 7: recompute the complete objective and validate layout,
    // union, projection, and feedback compatibility before returning.

    stratum
}
