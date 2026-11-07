//! Parser tests, split by grammar layer.

mod clause;
mod expression;
mod pattern;
mod statement;

use atlantis_core::Value;

use crate::ast::*;
use crate::{parse, parse_script};

/// Parses a single-query statement without `UNION`.
pub(super) fn query(src: &str) -> Query {
    match parse(src).unwrap_or_else(|e| panic!("{src}: {e}")) {
        Statement::Query(QueryUnion { first, rest }) if rest.is_empty() => first,
        other => panic!("{src}: not a single query: {other:?}"),
    }
}

/// The parse error message for `src`.
pub(super) fn parse_err(src: &str) -> String {
    match parse(src) {
        Ok(stmt) => panic!("{src}: expected an error, got {stmt:?}"),
        Err(e) => e.to_string(),
    }
}

/// The expression of `RETURN <src>`.
pub(super) fn expr(src: &str) -> Expression {
    let q = query(&format!("RETURN {src}"));
    let mut ret = q.ret.expect("RETURN projection");
    assert_eq!(ret.items.len(), 1, "{src}");
    ret.items.remove(0).expr
}

pub(super) fn var(name: &str) -> Expression {
    Expression::Variable(name.into())
}

pub(super) fn int(n: i64) -> Expression {
    Expression::Literal(Value::Integer(n))
}

pub(super) fn string(s: &str) -> Expression {
    Expression::Literal(Value::String(s.into()))
}

pub(super) fn prop(e: Expression, key: &str) -> Expression {
    Expression::Property(Box::new(e), key.into())
}

pub(super) fn bin(l: Expression, op: BinaryOp, r: Expression) -> Expression {
    Expression::Binary(Box::new(l), op, Box::new(r))
}

#[test]
fn script_splits_statements() {
    let stmts = parse_script("BEGIN; CREATE (n:A);; MATCH (n) RETURN n;\nCOMMIT;").unwrap();
    assert_eq!(stmts.len(), 4);
    assert_eq!(stmts[0], Statement::Begin);
    assert_eq!(stmts[3], Statement::Commit);
    assert!(parse_script("").unwrap().is_empty());
    assert!(parse_script(" ; ; // only comments\n").unwrap().is_empty());
}

#[test]
fn script_errors_report_position() {
    let e = parse_script("CREATE (n)\nRETURN n RETURN m")
        .unwrap_err()
        .to_string();
    assert!(e.contains("2:10") && e.contains("'RETURN'"), "{e}");
}

#[test]
fn single_statement_rules() {
    assert_eq!(parse("ROLLBACK;").unwrap(), Statement::Rollback);
    assert!(parse_err("BEGIN; COMMIT").contains("expected end of input"));
    assert!(parse_err("").contains("unexpected end of input"));
    assert!(parse_err("RETURN 1;;").contains("1:10"));
}
