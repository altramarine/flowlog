//! Reversible physical layouts for projected join outputs.

use flowlog_planner::planner::ArithmeticArgument;
use flowlog_planner::planner::FactorArgument;
use flowlog_planner::planner::TransformationArgument;

/// Splits a row projection into left-side keys and right-side values, with
/// a projection restoring the original row order. Both sides must contribute
/// bare value columns; retained join keys and computed outputs are excluded.
pub(super) struct JoinOutputLayout {
    pub(super) key: Vec<ArithmeticArgument>,
    pub(super) value: Vec<ArithmeticArgument>,
    pub(super) row: Vec<ArithmeticArgument>,
}

impl JoinOutputLayout {
    pub(super) fn from_projection(args: &[ArithmeticArgument]) -> Option<Self> {
        let mut key = Vec::new();
        let mut value = Vec::new();
        let mut row = Vec::new();
        for arg in args {
            if !arg.rest().is_empty() {
                return None;
            }
            let FactorArgument::Var(TransformationArgument::Jn((is_left, false, _))) = arg.init()
            else {
                return None;
            };
            let fields = if *is_left { &mut key } else { &mut value };
            row.push(ArithmeticArgument {
                init: FactorArgument::Var(TransformationArgument::KV((*is_left, fields.len()))),
                rest: Vec::new(),
            });
            fields.push(arg.clone());
        }
        (!key.is_empty() && !value.is_empty()).then_some(Self { key, value, row })
    }
}

#[cfg(test)]
mod tests {
    use flowlog_parser::ArithmeticOperator;
    use flowlog_planner::planner::ArithmeticArgument;
    use flowlog_planner::planner::FactorArgument;
    use flowlog_planner::planner::TransformationArgument;
    use rstest::rstest;

    use super::JoinOutputLayout;

    fn column(left: bool, key: bool, index: usize) -> ArithmeticArgument {
        ArithmeticArgument {
            init: FactorArgument::Var(TransformationArgument::Jn((left, key, index))),
            rest: Vec::new(),
        }
    }

    /// Physical grouping is private to codegen; this pins its reversible layout.
    #[test]
    fn grouping_restores_interleaved_and_repeated_columns() {
        let layout = JoinOutputLayout::from_projection(&[
            column(false, false, 2),
            column(true, false, 1),
            column(false, false, 0),
            column(true, false, 1),
        ])
        .unwrap();
        assert_eq!(
            layout.key,
            vec![column(true, false, 1), column(true, false, 1)]
        );
        assert_eq!(
            layout.value,
            vec![column(false, false, 2), column(false, false, 0)]
        );
        assert_eq!(
            layout
                .row
                .iter()
                .map(ArithmeticArgument::init)
                .collect::<Vec<_>>(),
            vec![
                &FactorArgument::Var(TransformationArgument::KV((false, 0))),
                &FactorArgument::Var(TransformationArgument::KV((true, 0))),
                &FactorArgument::Var(TransformationArgument::KV((false, 1))),
                &FactorArgument::Var(TransformationArgument::KV((true, 1))),
            ]
        );
    }

    #[rstest]
    #[case(vec![])]
    #[case(vec![column(true, false, 0)])]
    #[case(vec![column(false, false, 0)])]
    #[case(vec![column(true, true, 0), column(false, false, 0)])]
    #[case(vec![column(true, false, 0), column(false, true, 0)])]
    fn grouping_requires_values_from_both_sides(#[case] args: Vec<ArithmeticArgument>) {
        assert!(JoinOutputLayout::from_projection(&args).is_none());
    }

    #[test]
    fn computed_outputs_do_not_supply_join_side_grouping() {
        let mut expression = column(true, false, 0);
        expression
            .rest
            .push((ArithmeticOperator::Plus, column(false, false, 0).init));
        assert!(
            JoinOutputLayout::from_projection(&[expression, column(false, false, 1)]).is_none()
        );
    }
}
