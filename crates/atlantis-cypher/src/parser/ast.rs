//! Abstract syntax tree for Atlantis Cypher.
//!
//! The tree mirrors the surface language closely; the executor interprets
//! it clause by clause. Labels, relationship types and property keys are
//! plain strings: a node's first label is its record table.

use atlantis_core::{Direction, Value};

/// One statement of a script. Scripts separate statements with `;`.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// A read/write query, possibly several combined with `UNION [ALL]`.
    Query(QueryUnion),
    /// `CREATE VECTOR INDEX name [IF NOT EXISTS] FOR (n:Label) ON (n.prop)
    /// [OPTIONS {dimension: 3, metric: 'cosine', m: 16, efConstruction: 200}]`
    CreateVectorIndex(CreateVectorIndex),
    /// `DROP INDEX name [IF EXISTS]`
    DropIndex { name: String, if_exists: bool },
    /// `BEGIN` — starts an explicit transaction in a script.
    Begin,
    /// `COMMIT` — commits the explicit transaction.
    Commit,
    /// `ROLLBACK` — discards the explicit transaction.
    Rollback,
}

/// Queries combined with `UNION` (`all = false`) or `UNION ALL`.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryUnion {
    pub first: Query,
    /// Each following query and whether it was joined with `UNION ALL`.
    pub rest: Vec<(bool, Query)>,
}

/// A single query: clauses followed by an optional `RETURN`.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub clauses: Vec<Clause>,
    pub ret: Option<Projection>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Clause {
    /// `[OPTIONAL] MATCH pattern [WHERE expr]`
    Match {
        optional: bool,
        pattern: Pattern,
        where_clause: Option<Expression>,
    },
    /// `CREATE pattern`
    Create(Pattern),
    /// `MERGE path [ON CREATE SET ...] [ON MATCH SET ...]`
    Merge {
        path: PathPattern,
        on_create: Vec<SetItem>,
        on_match: Vec<SetItem>,
    },
    /// `SET item, item, ...`
    Set(Vec<SetItem>),
    /// `REMOVE item, item, ...`
    Remove(Vec<RemoveItem>),
    /// `[DETACH] DELETE expr, expr, ...`
    Delete {
        detach: bool,
        exprs: Vec<Expression>,
    },
    /// `WITH projection [WHERE expr]`
    With {
        projection: Projection,
        where_clause: Option<Expression>,
    },
    /// `UNWIND expr AS variable`
    Unwind { expr: Expression, variable: String },
    /// `CALL name(args) [YIELD a [AS b], ...] [WHERE expr]`
    Call {
        procedure: String,
        args: Vec<Expression>,
        /// `None` means no `YIELD`: all output columns are bound.
        yields: Option<Vec<YieldItem>>,
        where_clause: Option<Expression>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct YieldItem {
    pub column: String,
    pub alias: Option<String>,
}

/// The body of `RETURN` and `WITH`.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub distinct: bool,
    /// `RETURN *` / `WITH *` — keep all bound variables (then `items`).
    pub star: bool,
    pub items: Vec<ProjectionItem>,
    pub order_by: Vec<OrderItem>,
    pub skip: Option<Expression>,
    pub limit: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionItem {
    pub expr: Expression,
    /// `AS alias`; without it the column name is the source text of `expr`.
    pub alias: Option<String>,
    /// Source text of the expression, used as the default column name.
    pub text: String,
}

impl ProjectionItem {
    /// The output column name.
    pub fn name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.text)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderItem {
    pub expr: Expression,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetItem {
    /// `target.a.b = value` — target is a variable, path is non-empty.
    Property {
        variable: String,
        path: Vec<String>,
        value: Expression,
    },
    /// `n = {map}` — replace all fields.
    Replace { variable: String, value: Expression },
    /// `n += {map}` — merge fields.
    Merge { variable: String, value: Expression },
    /// `n:Label:Other` — add secondary labels.
    Labels {
        variable: String,
        labels: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RemoveItem {
    /// `n.a.b`
    Property { variable: String, path: Vec<String> },
    /// `n:Label`
    Labels {
        variable: String,
        labels: Vec<String>,
    },
}

/// Comma-separated path patterns.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub paths: Vec<PathPattern>,
}

/// `[p =] (a)-[r]->(b)<-[s]-(c) ...`
#[derive(Debug, Clone, PartialEq)]
pub struct PathPattern {
    pub variable: Option<String>,
    pub start: NodePattern,
    pub steps: Vec<(RelPattern, NodePattern)>,
}

/// `(var:Label:Other {key: expr, ...})`
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodePattern {
    pub variable: Option<String>,
    pub labels: Vec<String>,
    pub properties: Vec<(String, Expression)>,
}

/// `-[var:TYPE|OTHER *min..max {key: expr}]->`
#[derive(Debug, Clone, PartialEq)]
pub struct RelPattern {
    pub variable: Option<String>,
    /// Alternative types (`:A|B`); empty means any type.
    pub types: Vec<String>,
    /// `Outgoing` for `-[]->`, `Incoming` for `<-[]-`, `Both` for `-[]-`.
    pub direction: Direction,
    pub properties: Vec<(String, Expression)>,
    /// `Some` for variable-length relationships.
    pub length: Option<PathLength>,
}

/// `*`, `*2`, `*1..3`, `*..3`, `*2..`. `None` bounds are open; the
/// executor uses a minimum of 1 when `min` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathLength {
    pub min: Option<usize>,
    pub max: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    Literal(Value),
    Parameter(String),
    Variable(String),
    /// `expr.key`
    Property(Box<Expression>, String),
    /// `expr[index]`
    Index(Box<Expression>, Box<Expression>),
    /// `expr[from..to]` with either bound optional.
    Slice(
        Box<Expression>,
        Option<Box<Expression>>,
        Option<Box<Expression>>,
    ),
    /// `expr:Label:Other` as a predicate.
    HasLabels(Box<Expression>, Vec<String>),
    List(Vec<Expression>),
    Map(Vec<(String, Expression)>),
    Unary(UnaryOp, Box<Expression>),
    Binary(Box<Expression>, BinaryOp, Box<Expression>),
    /// `expr IS NULL` (`negated = false`) / `expr IS NOT NULL`.
    IsNull(Box<Expression>, bool),
    /// Function call; `name` keeps dots and original case for namespaced
    /// functions (`db.vector.similarity`). Matching is case-insensitive.
    Function {
        name: String,
        distinct: bool,
        args: Vec<Expression>,
    },
    /// `count(*)`
    CountStar,
    /// `CASE [operand] WHEN a THEN b ... [ELSE c] END`
    Case {
        operand: Option<Box<Expression>>,
        whens: Vec<(Expression, Expression)>,
        default: Option<Box<Expression>>,
    },
    /// `[x IN list [WHERE pred] [| expr]]`
    ListComprehension {
        variable: String,
        list: Box<Expression>,
        predicate: Option<Box<Expression>>,
        map: Option<Box<Expression>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
    Plus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    Xor,
    And,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    /// `=~` regular-expression match
    RegexMatch,
    In,
    StartsWith,
    EndsWith,
    Contains,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

impl Expression {
    /// `true` if the expression contains an aggregate function call.
    pub fn contains_aggregate(&self) -> bool {
        match self {
            Expression::CountStar => true,
            Expression::Function { name, args, .. } => {
                is_aggregate(name) || args.iter().any(Expression::contains_aggregate)
            }
            Expression::Property(e, _)
            | Expression::Unary(_, e)
            | Expression::IsNull(e, _)
            | Expression::HasLabels(e, _) => e.contains_aggregate(),
            Expression::Index(a, b) | Expression::Binary(a, _, b) => {
                a.contains_aggregate() || b.contains_aggregate()
            }
            Expression::Slice(e, a, b) => {
                e.contains_aggregate()
                    || a.as_ref().is_some_and(|x| x.contains_aggregate())
                    || b.as_ref().is_some_and(|x| x.contains_aggregate())
            }
            Expression::List(items) => items.iter().any(Expression::contains_aggregate),
            Expression::Map(items) => items.iter().any(|(_, e)| e.contains_aggregate()),
            Expression::Case {
                operand,
                whens,
                default,
            } => {
                operand.as_ref().is_some_and(|e| e.contains_aggregate())
                    || whens
                        .iter()
                        .any(|(a, b)| a.contains_aggregate() || b.contains_aggregate())
                    || default.as_ref().is_some_and(|e| e.contains_aggregate())
            }
            Expression::ListComprehension {
                list,
                predicate,
                map,
                ..
            } => {
                list.contains_aggregate()
                    || predicate.as_ref().is_some_and(|e| e.contains_aggregate())
                    || map.as_ref().is_some_and(|e| e.contains_aggregate())
            }
            Expression::Literal(_) | Expression::Parameter(_) | Expression::Variable(_) => false,
        }
    }
}

/// Names of aggregate functions (lower case).
pub const AGGREGATES: &[&str] = &["count", "sum", "avg", "min", "max", "collect", "stdev"];

/// `true` if `name` is an aggregate function.
pub fn is_aggregate(name: &str) -> bool {
    AGGREGATES.iter().any(|a| a.eq_ignore_ascii_case(name))
}

/// Parameters of `CREATE VECTOR INDEX`.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateVectorIndex {
    pub name: String,
    pub if_not_exists: bool,
    pub label: String,
    pub property: String,
    pub options: Vec<(String, Expression)>,
}
