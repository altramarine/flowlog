//! Physical arrangements and the expressions that build them.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;

use flowlog_parser::Relation;

use crate::planner::HeadOperation;
use crate::planner::Transformation;

// =============================================================================
// Scope
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Scope {
    NonRecursive,
    Recursive,
}

// =============================================================================
// Arities
// =============================================================================

/// Sortable key and value blocks, as column positions in the feed's
/// flattened output row. Aggregation contributions live in weights and
/// are excluded; the aggregation's result row still contains its answer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Arities {
    pub(super) key: Vec<usize>,
    pub(super) value: Vec<usize>,
}

impl Arities {
    /// Assigns contiguous slots to a key followed by its value.
    #[must_use]
    pub(super) fn new(key: usize, value: usize) -> Self {
        Self {
            key: (0..key).collect(),
            value: (key..key + value).collect(),
        }
    }
}

// =============================================================================
// PhysicalArrangement
// =============================================================================

/// One materialized layout, including dedup and aggregation results. Static
/// dedup is a sorting boundary even when consolidation retains no trace.
/// Stratum zero is current; later indices follow evaluation order.
/// `arities` names the field blocks whose internal orders can change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PhysicalArrangement {
    Arrangement {
        stratum: usize,
        scope: Scope,
        transformation: usize,
        arities: Arities,
    },
    /// All union feeds share this node's single dedup layout.
    Dedup {
        stratum: usize,
        scope: Scope,
        relation: u64,
        arities: Arities,
    },
    Aggregation {
        stratum: usize,
        scope: Scope,
        relation: u64,
        arities: Arities,
    },
}

impl PhysicalArrangement {
    #[must_use]
    pub(super) fn head(
        declaration: &Relation,
        operation: &HeadOperation,
        stratum: usize,
        scope: Scope,
    ) -> Self {
        let relation = declaration.fingerprint();
        let columns = declaration.arity();
        match operation {
            HeadOperation::Aggregation => {
                let target = declaration.lattice().map(|(target, _)| target);
                Self::Aggregation {
                    stratum,
                    scope,
                    relation,
                    arities: Arities {
                        key: (0..columns)
                            .filter(|&column| Some(column) != target)
                            .collect(),
                        value: Vec::new(),
                    },
                }
            }
            HeadOperation::Dedup { order } => Self::Dedup {
                stratum,
                scope,
                relation,
                arities: Arities {
                    key: order.clone(),
                    value: Vec::new(),
                },
            },
        }
    }
}

// =============================================================================
// Producer
// =============================================================================

/// A feed expression stops at an arrangement or an external collection.
/// Row-only steps retain their field mappings without becoming nodes.
/// Borrowed steps reference the plan; owned steps compose projections for
/// one feed without rewriting the shared collection's other consumers.
#[derive(Clone)]
pub(super) enum Producer<'plan> {
    External(u64),
    Arrangement(PhysicalArrangement),
    Transformation {
        step: Cow<'plan, Transformation>,
        sources: Vec<Producer<'plan>>,
    },
    Union(Vec<Producer<'plan>>),
}

impl fmt::Debug for Producer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::External(fingerprint) => {
                write!(f, "External(0x{fingerprint:016x})")
            }
            Self::Arrangement(node) => node.fmt(f),
            // The full step also prints collections and canonical forms;
            // only the operation, mappings and feeds belong in this graph.
            Self::Transformation { step, sources } => f
                .debug_struct(step.operation_name())
                .field("sources", sources)
                .field("flow", step.flow())
                .finish(),
            Self::Union(parts) => f.debug_tuple("Union").field(parts).finish(),
        }
    }
}

// =============================================================================
// Edge
// =============================================================================

/// One input expression builds one output arrangement. A join keeps both
/// ordered sources in its expression; a head keeps every union feed.
#[derive(Debug)]
pub(super) struct Edge<'plan> {
    pub(super) input: Producer<'plan>,
    pub(super) output: PhysicalArrangement,
}

// =============================================================================
// Graph
// =============================================================================

#[derive(Debug, Default)]
pub(super) struct Graph<'plan> {
    pub(super) nodes: BTreeSet<PhysicalArrangement>,
    pub(super) edges: Vec<Edge<'plan>>,
}

#[cfg(test)]
mod tests {
    use flowlog_common::compute_fp;
    use flowlog_parser::test_harness::program;
    use rstest::rstest;

    use super::*;

    // Cases: declaration fields, grouping columns.
    #[rstest]
    #[case("a: min<int32>, b: int32, c: int32", &[1, 2])]
    #[case("a: int32, b: min<int32>, c: int32", &[0, 2])]
    #[case("a: int32, b: int32, c: min<int32>", &[0, 1])]
    #[case("a: min<int32>", &[])]
    fn aggregation_arities_exclude_the_aggregated_column(
        #[case] fields: &str,
        #[case] expected_key: &[usize],
    ) {
        let program = program(&format!(
            r#"
            .decl R({fields})
            .input R(IO="file", filename="R.csv")
            .output R
            "#,
        ));
        let declaration = program.relation_by_fingerprint(compute_fp("r")).unwrap();
        let node = PhysicalArrangement::head(
            declaration,
            &HeadOperation::Aggregation,
            0,
            Scope::NonRecursive,
        );
        let PhysicalArrangement::Aggregation { arities, .. } = node else {
            panic!("a declared min column selects aggregation");
        };
        assert_eq!(arities.key, expected_key);
        assert!(arities.value.is_empty());
    }

    #[test]
    fn dedup_arities_include_every_row_column_in_one_block() {
        let program = program(
            r#"
            .decl R(a: int32, b: int32, c: int32)
            .input R(IO="file", filename="R.csv")
            .output R
            "#,
        );
        let declaration = program.relation_by_fingerprint(compute_fp("r")).unwrap();
        let node = PhysicalArrangement::head(
            declaration,
            &HeadOperation::Dedup {
                order: vec![0, 1, 2],
            },
            0,
            Scope::NonRecursive,
        );
        let PhysicalArrangement::Dedup { arities, .. } = node else {
            panic!("an ordinary relation selects dedup");
        };
        assert_eq!(arities.key, [0, 1, 2]);
        assert!(arities.value.is_empty());
    }
}
