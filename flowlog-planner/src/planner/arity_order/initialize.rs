//! Arrangement provenance across evaluation and recursion scopes.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::mem;
use std::sync::Arc;

use flowlog_parser::Program;

use super::graph::Arities;
use super::graph::Edge;
use super::graph::Graph;
use super::graph::PhysicalArrangement;
use super::graph::Producer;
use super::graph::Scope;
use crate::planner::ArithmeticArgument;
use crate::planner::FactorArgument;
use crate::planner::StratumPlanner;
use crate::planner::Transformation;
use crate::planner::TransformationArgument;
use crate::planner::TransformationFlow;

type Bindings<'plan> = BTreeMap<u64, Producer<'plan>>;

// =============================================================================
// Graph initialization
// =============================================================================

/// Preserves arrangement identities across sharing and scope entry, and
/// composes projections within feeds up to materialized layout boundaries.
/// Outputs of `upstream` are external inputs to this graph. `downstream`
/// is ordered from the last stratum to the nearest one.
#[must_use]
pub(super) fn initialize<'plan>(
    stratum: &'plan StratumPlanner,
    upstream: &[StratumPlanner],
    downstream: &'plan [StratumPlanner],
    program: &Program,
) -> Graph<'plan> {
    let mut graph = Graph::default();
    // A head can add rules to a relation already bound by an earlier
    // stratum. Preserve that binding even if no body reads the relation.
    let earlier_relations = upstream.iter().flat_map(StratumPlanner::output_relations);
    let mut bindings: Bindings<'plan> = program
        .edb_fingerprints()
        .into_iter()
        .chain(earlier_relations)
        .map(|fingerprint| (fingerprint, Producer::External(fingerprint)))
        .collect();
    initialize_stratum(0, stratum, program, &mut bindings, &mut graph);
    let evaluation_order = downstream.iter().rev().enumerate();
    for (index, next) in evaluation_order {
        initialize_stratum(index + 1, next, program, &mut bindings, &mut graph);
    }
    graph
}

/// Adds one stratum's arrangement graph without changing its plan.
/// `bindings` maps available outer collection fingerprints to their
/// producers. It gains prelude results and non-recursive heads; from a
/// recursive body, only the heads leaving the loop are published.
///
/// Each relation head is one dedup or aggregation node fed by its rule
/// results and any earlier seed. Recursive reads reference that head,
/// forming a graph cycle rather than unrolling iterations. The seed
/// retains its outer producer even when feedback names the same relation.
fn initialize_stratum<'plan>(
    index: usize,
    stratum: &'plan StratumPlanner,
    program: &Program,
    bindings: &mut Bindings<'plan>,
    graph: &mut Graph<'plan>,
) {
    // Heads and loop entries can read prelude results, so resolve those
    // producers before rebinding any relation to its head.
    initialize_transformations(
        index,
        Scope::NonRecursive,
        stratum.non_recursive_transformations(),
        bindings,
        None,
        graph,
    );

    if !stratum.is_recursive() {
        // All heads read the incoming bindings and the completed prelude.
        // Publish heads afterwards so map iteration cannot change lineage.
        let mut heads = BTreeMap::new();
        for declaration in program.relations() {
            let relation = declaration.fingerprint();
            let Some(head) = stratum.heads().get(&relation) else {
                continue;
            };
            let node = PhysicalArrangement::head(
                declaration,
                head.operation(),
                index,
                Scope::NonRecursive,
            );
            let mut parts: Vec<Producer<'plan>> = head
                .seed()
                .map(|seed| resolve_producer(seed, bindings, None))
                .into_iter()
                .collect();
            parts.extend(
                head.inputs()
                    .iter()
                    .map(|&fingerprint| resolve_producer(fingerprint, bindings, None)),
            );
            graph.nodes.insert(node.clone());
            graph.edges.push(Edge {
                input: Producer::Union(parts),
                output: node.clone(),
            });
            heads.insert(relation, Producer::Arrangement(node));
        }
        bindings.extend(heads);
        return;
    }

    // Codegen omits an unobservable loop but still emits its prelude.
    if stratum.recursion_leave_collections().is_empty() {
        return;
    }

    let entered: Bindings<'plan> = stratum
        .recursion_enter_collections()
        .iter()
        .map(|&fingerprint| (fingerprint, resolve_producer(fingerprint, bindings, None)))
        .collect();
    // Keep the seed bindings separate: feedback must shadow body reads
    // of R without replacing the outer R that feeds the head's union.
    let mut recursive_bindings = entered.clone();
    let mut heads = BTreeMap::new();
    // Reserve head identities before the body so its feedback sources
    // can reference nodes whose incoming edges have not been filled yet.
    for declaration in program.relations() {
        let relation = declaration.fingerprint();
        let Some(head) = stratum.heads().get(&relation) else {
            continue;
        };
        let node =
            PhysicalArrangement::head(declaration, head.operation(), index, Scope::Recursive);
        graph.nodes.insert(node.clone());
        heads.insert(relation, Producer::Arrangement(node));
    }
    // A recursive feed references the head arrangement directly; neither
    // entering a scope nor feeding back rows builds another arrangement.
    for &relation in stratum.recursion_feedback_collections() {
        if let Some(head) = heads.get(&relation) {
            recursive_bindings.insert(relation, head.clone());
        }
    }
    initialize_transformations(
        index,
        Scope::Recursive,
        stratum.recursive_transformations(),
        &mut recursive_bindings,
        Some(bindings),
        graph,
    );
    for declaration in program.relations() {
        let relation = declaration.fingerprint();
        let Some(head) = stratum.heads().get(&relation) else {
            continue;
        };
        let node =
            PhysicalArrangement::head(declaration, head.operation(), index, Scope::Recursive);
        let mut parts: Vec<Producer<'plan>> = head
            .seed()
            .and_then(|seed| entered.get(&seed))
            .cloned()
            .into_iter()
            .collect();
        parts.extend(head.inputs().iter().map(|&fingerprint| {
            resolve_producer(fingerprint, &recursive_bindings, Some(bindings))
        }));
        graph.edges.push(Edge {
            input: Producer::Union(parts),
            output: node,
        });
    }
    // Loop-local arrangements must not alias later arrangements of the
    // completed rows, so only leaving heads become outer bindings.
    for &relation in stratum.recursion_leave_collections() {
        if let Some(head) = heads.get(&relation) {
            bindings.insert(relation, head.clone());
        }
    }
}

/// Composes projection feeds until a keyed output materializes them.
/// Binary sources retain their left-to-right order.
fn initialize_transformations<'plan>(
    stratum: usize,
    scope: Scope,
    transformations: &'plan [Transformation],
    bindings: &mut Bindings<'plan>,
    outer: Option<&Bindings<'plan>>,
    graph: &mut Graph<'plan>,
) {
    let steps = transformations.iter().enumerate();
    for (transformation, step) in steps {
        let inputs = step.input_fingerprints();
        let sources: Vec<Producer<'plan>> = inputs
            .into_iter()
            .map(|fingerprint| resolve_producer(fingerprint, bindings, outer))
            .collect();
        let expression = fuse_projections(step, sources);
        let output = if step.need_arrange() {
            let (key, value) = step.output().arity();
            let node = PhysicalArrangement::Arrangement {
                stratum,
                scope,
                transformation,
                arities: Arities::new(key, value),
            };
            graph.nodes.insert(node.clone());
            graph.edges.push(Edge {
                input: expression,
                output: node.clone(),
            });
            Producer::Arrangement(node)
        } else {
            expression
        };
        bindings.insert(step.output().fingerprint(), output);
    }
}

/// Resolves local bindings before outer bindings. Unresolved collections
/// remain external leaves instead of introducing arrangement nodes.
#[must_use]
fn resolve_producer<'plan>(
    fingerprint: u64,
    bindings: &Bindings<'plan>,
    outer: Option<&Bindings<'plan>>,
) -> Producer<'plan> {
    bindings
        .get(&fingerprint)
        .or_else(|| outer.and_then(|bindings| bindings.get(&fingerprint)))
        .cloned()
        .unwrap_or(Producer::External(fingerprint))
}

// =============================================================================
// Projection fusion
// =============================================================================

/// Collapses pure projection chains within a feed. Physical arrangements,
/// filters and computed expressions stop fusion. Shared feeds stay intact.
#[must_use]
fn fuse_projections<'plan>(
    step: &'plan Transformation,
    mut sources: Vec<Producer<'plan>>,
) -> Producer<'plan> {
    let mut step = Cow::Borrowed(step);
    while step.is_map() {
        let [
            Producer::Transformation {
                step: previous,
                sources: previous_sources,
            },
        ] = sources.as_mut_slice()
        else {
            break;
        };
        let (input, row_input) = match previous.as_ref() {
            Transformation::RowToRow { input, .. } => (input, true),
            Transformation::KvToRow { input, .. } => (input, false),
            Transformation::RowToKv { .. }
            | Transformation::KvToKv { .. }
            | Transformation::JnToRow { .. }
            | Transformation::JnToKv { .. }
            | Transformation::NJnToRow { .. }
            | Transformation::NJnToKv { .. }
            | Transformation::AggToRow { .. }
            | Transformation::AggToKv { .. } => break,
        };
        let Some(flow) = compose_projection(step.flow(), previous.flow()) else {
            break;
        };
        let input = Arc::clone(input);
        let output = Arc::clone(step.output());
        let arranged_output = step.need_arrange();
        step = Cow::Owned(match (row_input, arranged_output) {
            (true, false) => Transformation::RowToRow {
                input,
                output,
                flow,
            },
            (true, true) => Transformation::RowToKv {
                input,
                output,
                flow,
            },
            (false, false) => Transformation::KvToRow {
                input,
                output,
                flow,
            },
            (false, true) => Transformation::KvToKv {
                input,
                output,
                flow,
            },
        });
        sources = mem::take(previous_sources);
    }
    Producer::Transformation { step, sources }
}

/// Resolves two column-only maps into one mapping over the earlier
/// input. Predicates and computed expressions cannot be elided here.
#[must_use]
fn compose_projection(
    projection: &TransformationFlow,
    previous: &TransformationFlow,
) -> Option<TransformationFlow> {
    let TransformationFlow::KVToKV {
        key,
        value,
        constraints,
        compares,
    } = projection
    else {
        return None;
    };
    let TransformationFlow::KVToKV {
        key: previous_key,
        value: previous_value,
        constraints: previous_constraints,
        compares: previous_compares,
    } = previous
    else {
        return None;
    };
    if !constraints.is_empty()
        || !compares.is_empty()
        || !previous_constraints.is_empty()
        || !previous_compares.is_empty()
    {
        return None;
    }
    let column_only = |argument: &ArithmeticArgument| {
        argument.rest.is_empty()
            && matches!(
                argument.init,
                FactorArgument::Var(TransformationArgument::KV(_))
            )
    };
    if !previous_key
        .iter()
        .chain(previous_value.iter())
        .all(column_only)
    {
        return None;
    }
    let resolve_column = |argument: &ArithmeticArgument| {
        if !argument.rest.is_empty() {
            return None;
        }
        let FactorArgument::Var(TransformationArgument::KV((is_key, index))) = &argument.init
        else {
            return None;
        };
        let fields = if *is_key {
            previous_key
        } else {
            previous_value
        };
        fields.get(*index).cloned()
    };
    let key: Option<Vec<ArithmeticArgument>> = key.iter().map(resolve_column).collect();
    let value: Option<Vec<ArithmeticArgument>> = value.iter().map(resolve_column).collect();
    Some(TransformationFlow::KVToKV {
        key: Arc::new(key?),
        value: Arc::new(value?),
        constraints: constraints.clone(),
        compares: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use flowlog_common::compute_fp;
    use flowlog_parser::test_harness::program;

    use super::*;
    use crate::planner::HeadOperation;
    use crate::planner::ProgramPlanner;

    /// Sharing reads S0's (b,c) stream to build S1's (c) index. Fuse that
    /// path while preserving the (b,c) feed into Mid's dedup.
    #[test]
    fn cross_stratum_projections_compose_without_changing_shared_feeds() {
        let program = program(
            r#"
            .decl R(k: int32, a: int32)
            .input R(IO="file", filename="R.csv")
            .decl S(k: int32, b: int32, c: int32)
            .input S(IO="file", filename="S.csv")
            .decl Wide(a: int32, b: int32, c: int32)
            .decl Mid(b: int32, c: int32)
            .decl Out(c: int32, z: int32)
            .output Wide
            .output Out
            Wide(a,b,c) :- R(k,a), S(k,b,c).
            Mid(b,c) :- R(k,_), S(k,b,c).
            Out(c,z) :- R(k,_), S(k,_,c), Mid(c,z).
            "#,
        );
        let plan = ProgramPlanner::from_program(&program, &mut None).expect("planned");
        let [upstream, downstream] = plan.strata() else {
            panic!("the Mid dependency separates two strata");
        };
        let graph = initialize(upstream, &[], std::slice::from_ref(downstream), &program);
        let index = graph
            .edges
            .iter()
            .find(|edge| {
                matches!(
                    edge.output,
                    PhysicalArrangement::Arrangement { stratum: 1, .. }
                ) && matches!(
                    &edge.input,
                    Producer::Transformation { step, .. } if step.output().is_k_only()
                )
            })
            .expect("the downstream c index");
        let Producer::Transformation { step, sources } = &index.input else {
            panic!("the c index has a projection feed");
        };
        assert_eq!(step.unary_input().arity(), (0, 3));
        assert_eq!(
            step.flow().key().as_slice(),
            &[ArithmeticArgument {
                init: FactorArgument::Var(TransformationArgument::KV((false, 2))),
                rest: Vec::new(),
            }],
        );
        assert!(step.flow().value().is_empty());
        let [Producer::Transformation { step: join, .. }] = sources.as_slice() else {
            panic!("the projection reads the shared join directly");
        };
        assert!(matches!(join.as_ref(), Transformation::JnToRow { .. }));

        let mid = graph
            .edges
            .iter()
            .find(|edge| {
                edge.output
                    == PhysicalArrangement::Dedup {
                        stratum: 0,
                        scope: Scope::NonRecursive,
                        relation: compute_fp("mid"),
                        arities: Arities {
                            key: vec![0, 1],
                            value: vec![],
                        },
                    }
            })
            .expect("Mid's dedup");
        let Producer::Union(parts) = &mid.input else {
            panic!("Mid's head retains its union feeds");
        };
        let [Producer::Transformation { step, .. }] = parts.as_slice() else {
            panic!("Mid has one projection feed");
        };
        assert_eq!(
            step.flow().value().as_slice(),
            &[
                ArithmeticArgument {
                    init: FactorArgument::Var(TransformationArgument::KV((false, 1))),
                    rest: Vec::new(),
                },
                ArithmeticArgument {
                    init: FactorArgument::Var(TransformationArgument::KV((false, 2))),
                    rest: Vec::new(),
                },
            ],
        );
    }

    #[test]
    fn dedup_arities_follow_the_order_written_back_to_the_stratum() {
        let program = program(
            r#"
            .decl E(a: int32, b: int32, c: int32)
            .input E
            .decl R(a: int32, b: int32, c: int32)
            .output R
            R(a,b,c) :- E(a,b,c).
            "#,
        );
        let stratifier = crate::stratifier::Stratifier::from_program(&program).expect("stratified");
        let mut optimizer = crate::optimizer::Optimizer::new();
        let declarations: std::collections::HashMap<u64, &flowlog_parser::Relation> = program
            .relations()
            .iter()
            .map(|relation| (relation.fingerprint(), relation))
            .collect();
        let mut planned = StratumPlanner::from_stratum(
            &program,
            &stratifier.strata()[0],
            &mut optimizer,
            &mut None,
            &[],
            &declarations,
        )
        .expect("planned");
        planned.bind_head_seeds(&program.edb_fingerprints());
        let stratum = &mut planned;
        stratum.heads.get_mut(&compute_fp("r")).unwrap().operation = HeadOperation::Dedup {
            order: vec![2, 0, 1],
        };
        let graph = initialize(stratum, &[], &[], &program);
        assert!(graph.nodes.contains(&PhysicalArrangement::Dedup {
            stratum: 0,
            scope: Scope::NonRecursive,
            relation: compute_fp("r"),
            arities: Arities {
                key: vec![2, 0, 1],
                value: vec![]
            },
        }));
    }

    #[test]
    fn a_later_stratums_head_keeps_earlier_contributions() {
        let program = program(
            r#"
            .decl E(x: int32)
            .input E(IO="file", filename="E.csv")
            .decl F(x: int32)
            .input F(IO="file", filename="F.csv")
            .decl G(x: int32)
            .input G(IO="file", filename="G.csv")
            .decl Aux(x: int32)
            .decl R(x: int32)
            .output R
            Aux(x) :- G(x).
            R(x) :- E(x).
            R(x) :- Aux(x), F(x).
            "#,
        );
        let plan = ProgramPlanner::from_program(&program, &mut None).expect("planned");
        let [upstream, downstream] = plan.strata() else {
            panic!("the Aux dependency separates two strata");
        };
        let graph = initialize(downstream, std::slice::from_ref(upstream), &[], &program);
        let head = graph
            .edges
            .iter()
            .find(|edge| {
                edge.output
                    == PhysicalArrangement::Dedup {
                        stratum: 0,
                        scope: Scope::NonRecursive,
                        relation: compute_fp("r"),
                        arities: Arities {
                            key: vec![0],
                            value: vec![],
                        },
                    }
            })
            .expect("R's later dedup");
        let Producer::Union(parts) = &head.input else {
            panic!("R's head retains its union feeds");
        };
        let [
            Producer::External(seed),
            Producer::Transformation { step, .. },
        ] = parts.as_slice()
        else {
            panic!("R's head must union its earlier binding with the new join");
        };
        assert_eq!(*seed, compute_fp("r"));
        assert!(matches!(step.as_ref(), Transformation::JnToRow { .. }));
    }

    #[test]
    fn projection_fusion_stops_at_a_dedup_arrangement() {
        let program = program(
            r#"
            .decl R(a: int32, b: int32, c: int32)
            .input R(IO="file", filename="R.csv")
            .decl P(b: int32, c: int32)
            .decl Out(c: int32)
            .output Out
            P(b,c) :- R(_,b,c).
            Out(c) :- P(_,c).
            "#,
        );
        let plan = ProgramPlanner::from_program(&program, &mut None).expect("planned");
        let [upstream, downstream] = plan.strata() else {
            panic!("the P dependency separates two strata");
        };
        let graph = initialize(upstream, &[], std::slice::from_ref(downstream), &program);
        let out = graph
            .edges
            .iter()
            .find(|edge| {
                edge.output
                    == PhysicalArrangement::Dedup {
                        stratum: 1,
                        scope: Scope::NonRecursive,
                        relation: compute_fp("out"),
                        arities: Arities {
                            key: vec![0],
                            value: vec![],
                        },
                    }
            })
            .expect("Out's dedup");
        let Producer::Union(parts) = &out.input else {
            panic!("Out's head retains its union feeds");
        };
        let [Producer::Transformation { step, sources }] = parts.as_slice() else {
            panic!("Out has one projection feed");
        };
        assert_eq!(step.unary_input().arity(), (0, 2));
        assert!(matches!(
            sources.as_slice(),
            [Producer::Arrangement(PhysicalArrangement::Dedup {
                stratum: 0,
                scope: Scope::NonRecursive,
                relation,
                ..
            })] if *relation == compute_fp("p")
        ));
    }
}
