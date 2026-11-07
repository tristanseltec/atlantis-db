//! Logical plan operator tree.

use atlantis_core::{Direction, Label, RelType};

use crate::parser::ast::Expression;

/// A logical operator in the query execution plan.
#[derive(Debug, Clone)]
pub enum LogicalOperator {
    /// Empty plan (no operations).
    Empty,

    /// A sequence of operators to execute in order.
    Sequence(Vec<LogicalOperator>),

    /// Scan all nodes with a given label.
    NodeByLabelScan {
        var: String,
        label: Label,
        input: Option<Box<LogicalOperator>>,
    },

    /// Scan all nodes.
    AllNodesScan {
        var: String,
        input: Option<Box<LogicalOperator>>,
    },

    /// Expand from one node to another via a relationship.
    Expand {
        from: String,
        to: String,
        rel_var: String,
        rel_type: Option<RelType>,
        direction: Direction,
        input: Box<LogicalOperator>,
    },

    /// Variable-length expand (multi-hop).
    VarLenExpand {
        from: String,
        to: String,
        rel_var: String,
        rel_type: Option<RelType>,
        direction: Direction,
        min: usize,
        max: usize,
        input: Box<LogicalOperator>,
    },

    /// Vector similarity search via HNSW index.
    VectorSearch {
        label: Label,
        query: Expression,
        k: usize,
        threshold: Option<f32>,
        node_var: String,
        score_var: String,
    },

    /// Filter rows by a predicate.
    Filter {
        input: Box<LogicalOperator>,
        predicate: Expression,
    },

    /// Project (select) specific expressions.
    Project {
        input: Box<LogicalOperator>,
        items: Vec<(Expression, Option<String>)>,
    },

    /// Sort rows by keys.
    Sort {
        input: Box<LogicalOperator>,
        keys: Vec<(Expression, bool)>,
    },

    /// Limit results to a count.
    Limit {
        input: Box<LogicalOperator>,
        count: Expression,
    },

    /// Skip a number of results.
    Skip {
        input: Box<LogicalOperator>,
        count: Expression,
    },

    /// Create a new node.
    CreateNode {
        var: String,
        labels: Vec<Label>,
        properties: Vec<(String, Expression)>,
    },

    /// Create a new relationship.
    CreateRelationship {
        from: String,
        to: String,
        rel_var: String,
        rel_type: RelType,
        properties: Vec<(String, Expression)>,
    },

    /// Set a property on a node or relationship.
    SetProperty {
        input: Box<LogicalOperator>,
        target: Expression,
        key: String,
        value: Expression,
    },

    /// Delete nodes or relationships.
    Delete {
        input: Box<LogicalOperator>,
        targets: Vec<Expression>,
        detach: bool,
    },

    /// Unwind a list into individual rows.
    Unwind {
        input: Box<LogicalOperator>,
        expression: Expression,
        variable: String,
    },
}
