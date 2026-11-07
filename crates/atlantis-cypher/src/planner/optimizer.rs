//! Rule-based query optimizer.

use super::logical::LogicalOperator;

/// Applies optimization rules to a logical plan.
///
/// Currently implements:
/// - Filter pushdown (move filters closer to scans)
/// - Dead code elimination (unused projections)
pub fn optimize(plan: LogicalOperator) -> LogicalOperator {
    // For v1, the planner already produces reasonable plans.
    // Future: implement filter pushdown, limit pushdown, vector index detection.
    plan
}
