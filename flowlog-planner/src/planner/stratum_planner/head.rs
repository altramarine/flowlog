//! Relation heads: their union inputs and the operation binding the result.

use flowlog_parser::Relation;

// =============================================================================
// HeadOperation
// =============================================================================

/// The operation applied after a relation's inputs are unioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadOperation {
    /// Sorts and dedups whole rows in `order`, then restores their logical
    /// column positions. `order` is a permutation of every row column.
    Dedup { order: Vec<usize> },
    /// Merges the declared min/max column by every other column. Its
    /// idempotent merge consumes the union without a whole-row dedup.
    Aggregation,
}

// =============================================================================
// HeadPlan
// =============================================================================

/// A relation's head after sharing: the earlier binding, when present,
/// followed by the rule results, and the operation binding their union.
#[derive(Debug)]
pub struct HeadPlan {
    pub(in crate::planner) inputs: Vec<u64>,
    pub(in crate::planner) seed: Option<u64>,
    pub(in crate::planner) operation: HeadOperation,
}

impl HeadPlan {
    #[must_use]
    pub(super) fn new(declaration: &Relation, inputs: Vec<u64>) -> Self {
        let operation = if declaration.lattice().is_some() {
            HeadOperation::Aggregation
        } else {
            HeadOperation::Dedup {
                order: (0..declaration.arity()).collect(),
            }
        };
        Self {
            inputs,
            seed: None,
            operation,
        }
    }

    /// Rule-result fingerprints after sharing, in concatenation order.
    #[must_use]
    pub fn inputs(&self) -> &[u64] {
        &self.inputs
    }

    /// The earlier outer binding unioned before the rule results. Inside
    /// recursion this is the entered seed, not the feedback variable.
    #[must_use]
    pub fn seed(&self) -> Option<u64> {
        self.seed
    }

    #[must_use]
    pub fn operation(&self) -> &HeadOperation {
        &self.operation
    }
}

#[cfg(test)]
mod tests {
    use flowlog_common::compute_fp;

    use super::*;
    use crate::test_harness::program_planner;

    #[test]
    fn ordinary_heads_plan_a_whole_row_dedup() {
        let plan = program_planner(
            r#"
            .decl E(a: int32, b: symbol)
            .input E
            .decl R(a: int32, b: symbol)
            .output R
            R(a,b) :- E(a,b).
            "#,
        );
        let head = &plan.strata()[0].heads()[&compute_fp("r")];
        assert_eq!(
            head.operation(),
            &HeadOperation::Dedup { order: vec![0, 1] }
        );
        assert_eq!(head.seed(), None);
        assert_eq!(head.inputs().len(), 1);
    }

    #[test]
    fn declared_aggregation_heads_do_not_plan_a_whole_row_dedup() {
        let plan = program_planner(
            r#"
            .decl E(a: int32, b: int32)
            .input E
            .decl R(a: int32, b: min<int32>)
            .output R
            R(a,b) :- E(a,b).
            "#,
        );
        let head = &plan.strata()[0].heads()[&compute_fp("r")];
        assert_eq!(head.operation(), &HeadOperation::Aggregation);
    }

    #[test]
    fn a_recursive_head_keeps_its_outer_seed_apart_from_feedback() {
        let plan = program_planner(
            r#"
            .decl Arc(x: int32, y: int32)
            .input Arc
            .decl Reach(x: int32, y: int32)
            .output Reach
            Reach(x,y) :- Arc(x,y).
            Reach(x,z) :- Reach(x,y), Arc(y,z).
            "#,
        );
        let [base, recursive] = plan.strata() else {
            panic!("Reach has a base stratum and a recursive stratum");
        };
        let relation = compute_fp("reach");
        assert_eq!(base.heads()[&relation].seed(), None);
        let head = &recursive.heads()[&relation];
        assert_eq!(head.seed(), Some(relation));
        assert!(!head.inputs().contains(&relation));
        assert!(
            recursive
                .recursion_feedback_collections()
                .contains(&relation)
        );
    }

    #[test]
    fn a_head_unions_its_existing_input_relation() {
        let plan = program_planner(
            r#"
            .decl E(a: int32)
            .input E
            .decl R(a: int32)
            .input R
            .output R
            R(a) :- E(a).
            "#,
        );
        assert_eq!(
            plan.strata()[0].heads()[&compute_fp("r")].seed(),
            Some(compute_fp("r"))
        );
    }
}
