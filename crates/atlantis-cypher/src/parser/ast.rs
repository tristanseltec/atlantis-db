//! Abstract Syntax Tree types for Cypher queries.

use atlantis_core::{Direction, Label, RelType, Value};
use serde::{Deserialize, Serialize};

/// A parsed Cypher statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// A read or write query.
    Query(Query),
}

/// A Cypher query: a sequence of clauses ending with RETURN.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// The clauses in this query (MATCH, WHERE, CREATE, etc.).
    pub clauses: Vec<Clause>,
    /// The final RETURN clause.
    pub result: Option<ReturnClause>,
}

/// A single clause in a Cypher query.
#[derive(Debug, Clone, PartialEq)]
pub enum Clause {
    Match(MatchClause),
    Where(Expression),
    Create(CreateClause),
    Merge(MergeClause),
    Delete(DeleteClause),
    Set(SetClause),
    Remove(RemoveClause),
    With(WithClause),
    Unwind(UnwindClause),
    Call(CallClause),
}

/// A MATCH clause for graph pattern matching.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchClause {
    /// The pattern to match.
    pub pattern: Pattern,
    /// Whether this is an OPTIONAL MATCH.
    pub optional: bool,
}

/// A CREATE clause for creating nodes and relationships.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateClause {
    /// The pattern to create.
    pub pattern: Pattern,
}

/// A MERGE clause for upsert operations.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeClause {
    /// The pattern to merge.
    pub pattern: Pattern,
}

/// A DELETE clause for removing nodes or relationships.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteClause {
    /// Whether to cascade-delete relationships (DETACH DELETE).
    pub detach: bool,
    /// Expressions identifying what to delete.
    pub expressions: Vec<Expression>,
}

/// A SET clause for updating properties.
#[derive(Debug, Clone, PartialEq)]
pub struct SetClause {
    /// A list of property assignments.
    pub assignments: Vec<SetAssignment>,
}

/// A single SET assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct SetAssignment {
    /// The property to set (e.g., `n.name`).
    pub target: Expression,
    /// The value to set.
    pub value: Expression,
}

/// A REMOVE clause for removing properties or labels.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoveClause {
    /// The items to remove.
    pub items: Vec<RemoveItem>,
}

/// A single REMOVE target.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoveItem {
    /// Remove a property from a node.
    Property(Expression, String),
    /// Remove a label from a node.
    Label(Expression, Label),
}

/// A WITH clause for query chaining.
#[derive(Debug, Clone, PartialEq)]
pub struct WithClause {
    /// Whether to apply DISTINCT.
    pub distinct: bool,
    /// The return items to pipe through.
    pub items: Vec<ReturnItem>,
    /// Optional ORDER BY specification.
    pub order_by: Option<Vec<OrderItem>>,
    /// Optional SKIP count.
    pub skip: Option<Expression>,
    /// Optional LIMIT count.
    pub limit: Option<Expression>,
    /// Optional WHERE filter.
    pub where_clause: Option<Expression>,
}

/// An UNWIND clause for expanding a list into rows.
#[derive(Debug, Clone, PartialEq)]
pub struct UnwindClause {
    /// The expression to unwind (must be a list).
    pub expression: Expression,
    /// The variable name to bind each element to.
    pub variable: String,
}

/// A CALL clause for vector search and other procedures.
#[derive(Debug, Clone, PartialEq)]
pub struct CallClause {
    /// The procedure name (e.g., "db.vector.search").
    pub procedure: String,
    /// The arguments to the procedure.
    pub arguments: Vec<Expression>,
    /// The YIELD variables (e.g., ["node", "score"]).
    pub yield_items: Option<Vec<(String, Option<String>)>>,
}

/// A RETURN clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    /// Whether to apply DISTINCT.
    pub distinct: bool,
    /// The items to return.
    pub items: Vec<ReturnItem>,
    /// Optional ORDER BY specification.
    pub order_by: Option<Vec<OrderItem>>,
    /// Optional SKIP count.
    pub skip: Option<Expression>,
    /// Optional LIMIT count.
    pub limit: Option<Expression>,
}

/// A single return item (expression with optional alias).
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    /// The expression to return.
    pub expression: Expression,
    /// Optional alias (AS name).
    pub alias: Option<String>,
}

/// A single ORDER BY item.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderItem {
    /// The expression to sort by.
    pub expression: Expression,
    /// Whether to sort descending.
    pub descending: bool,
}

/// A graph pattern (sequence of node and relationship patterns).
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// The node patterns and relationship patterns, interleaved.
    pub elements: Vec<PatternElement>,
}

/// An element in a graph pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternElement {
    /// A node pattern.
    Node(NodePattern),
    /// A relationship pattern with its direction.
    Relationship(RelationshipPattern),
}

/// A node pattern (e.g., `(n:Person {name: 'Alice'})`).
#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    /// The variable name bound to this node (if any).
    pub variable: Option<String>,
    /// The labels on the node (e.g., `:Person:Adult`).
    pub labels: Vec<Label>,
    /// Inline properties `{key: value}`.
    pub properties: Vec<(String, Expression)>,
}

/// A relationship pattern (e.g., `-[r:KNOWS {since: 2020}]->`).
#[derive(Debug, Clone, PartialEq)]
pub struct RelationshipPattern {
    /// The variable name bound to this relationship (if any).
    pub variable: Option<String>,
    /// The relationship type (if specified).
    pub rel_type: Option<RelType>,
    /// The direction of the relationship.
    pub direction: Direction,
    /// Inline properties.
    pub properties: Vec<(String, Expression)>,
    /// Variable-length path specification (*min..max), if any.
    pub length: Option<PathLength>,
}

/// Variable-length path specification.
#[derive(Debug, Clone, PartialEq)]
pub struct PathLength {
    /// Minimum number of hops (default: 1).
    pub min: Option<usize>,
    /// Maximum number of hops (None = unlimited).
    pub max: Option<usize>,
}

/// A Cypher expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    /// A variable reference (e.g., `n`).
    Variable(String),
    /// Property access (e.g., `n.name`).
    Property(Box<Expression>, String),
    /// A literal value.
    Literal(Value),
    /// A parameter reference (e.g., `$name`).
    Parameter(String),
    /// A function call (e.g., `count(*)`, `db.vector.search(...)`).
    FunctionCall(String, Vec<Expression>),
    /// A binary operation.
    BinaryOp {
        left: Box<Expression>,
        op: BinaryOp,
        right: Box<Expression>,
    },
    /// A unary operation.
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expression>,
    },
    /// A list literal.
    List(Vec<Expression>),
    /// A map literal (key-value pairs).
    Map(Vec<(String, Expression)>),
    /// A CASE expression.
    Case {
        /// The WHEN/THEN pairs.
        cases: Vec<(Expression, Expression)>,
        /// The optional ELSE value.
        default: Option<Box<Expression>>,
    },
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinaryOp {
    /// `=`
    Eq,
    /// `<>`
    NotEq,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    LtEq,
    /// `>=`
    GtEq,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `AND`
    And,
    /// `OR`
    Or,
    /// `XOR`
    Xor,
    /// `IN`
    In,
    /// `STARTS WITH`
    StartsWith,
    /// `ENDS WITH`
    EndsWith,
    /// `CONTAINS`
    Contains,
    /// `IS NULL`
    IsNull,
    /// `IS NOT NULL`
    IsNotNull,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    /// Unary negation (`-x`).
    Neg,
    /// Logical NOT (`NOT x`).
    Not,
}
