use atlantis_core::Direction;

use super::{int, parse_err, query, string};
use crate::ast::{Clause, NodePattern, PathLength, PathPattern, Pattern, RelPattern};

/// The pattern of the first clause of `MATCH <src> RETURN 1`.
fn pattern(src: &str) -> Pattern {
    match query(&format!("MATCH {src} RETURN 1")).clauses.remove(0) {
        Clause::Match { pattern, .. } => pattern,
        other => panic!("{other:?}"),
    }
}

fn path(src: &str) -> PathPattern {
    let mut p = pattern(src);
    assert_eq!(p.paths.len(), 1);
    p.paths.remove(0)
}

fn rels(src: &str) -> Vec<RelPattern> {
    path(src).steps.into_iter().map(|(r, _)| r).collect()
}

fn node(variable: Option<&str>, labels: &[&str]) -> NodePattern {
    NodePattern {
        variable: variable.map(Into::into),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        properties: vec![],
    }
}

#[test]
fn nodes() {
    assert_eq!(path("()").start, NodePattern::default());
    assert_eq!(path("(n)").start, node(Some("n"), &[]));
    assert_eq!(path("(:A)").start, node(None, &["A"]));
    assert_eq!(
        path("(n:Person:Admin)").start,
        node(Some("n"), &["Person", "Admin"])
    );
    assert_eq!(path("(order:Order)").start, node(Some("order"), &["Order"]));
    assert_eq!(
        path("(`my var`:`My Label`)").start,
        node(Some("my var"), &["My Label"])
    );
    assert_eq!(
        path("(n:P {name: 'a', age: 3})").start.properties,
        vec![
            ("name".to_string(), string("a")),
            ("age".to_string(), int(3))
        ]
    );
}

#[test]
fn directions_including_shorthand() {
    let dirs = |src: &str| {
        rels(src)
            .into_iter()
            .map(|r| r.direction)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        dirs("(a)-->(b)<--(c)--(d)"),
        vec![Direction::Outgoing, Direction::Incoming, Direction::Both]
    );
    assert_eq!(
        dirs("(a)-[]->(b)<-[]-(c)-[]-(d)"),
        vec![Direction::Outgoing, Direction::Incoming, Direction::Both]
    );
    assert_eq!(dirs("(a)<-[r]->(b)"), vec![Direction::Both]);
    assert_eq!(dirs("(a)<-->(b)"), vec![Direction::Both]);
}

#[test]
fn relationship_details() {
    let r = rels("(a)-[r:KNOWS|`LIKES`|:HATES {since: 2020}]->(b)").remove(0);
    assert_eq!(r.variable.as_deref(), Some("r"));
    assert_eq!(r.types, vec!["KNOWS", "LIKES", "HATES"]);
    assert_eq!(r.properties, vec![("since".to_string(), int(2020))]);
    assert_eq!(r.length, None);
    let r = rels("(a)-[:TYPE]-(b)").remove(0);
    assert_eq!((r.variable, r.types), (None, vec!["TYPE".to_string()]));
}

#[test]
fn variable_lengths() {
    let len = |spec: &str| rels(&format!("(a)-[r:T {spec}]->(b)")).remove(0).length;
    let pl = |min, max| Some(PathLength { min, max });
    assert_eq!(len("*"), pl(None, None));
    assert_eq!(len("*3"), pl(Some(3), Some(3)));
    assert_eq!(len("*1..3"), pl(Some(1), Some(3)));
    assert_eq!(len("*..3"), pl(None, Some(3)));
    assert_eq!(len("*2.."), pl(Some(2), None));
    assert_eq!(
        rels("(a)-[*1..2 {w: 1}]-(b)").remove(0).properties,
        vec![("w".to_string(), int(1))]
    );
}

#[test]
fn paths_and_path_variables() {
    let p = pattern("p = (a)-[:R]->(b), (c), q=(d)<--(e)");
    assert_eq!(p.paths.len(), 3);
    assert_eq!(p.paths[0].variable.as_deref(), Some("p"));
    assert_eq!(p.paths[0].steps[0].1, node(Some("b"), &[]));
    assert_eq!(p.paths[1].variable, None);
    assert_eq!(p.paths[2].variable.as_deref(), Some("q"));
    assert_eq!(p.paths[2].steps[0].0.direction, Direction::Incoming);
}

#[test]
fn pattern_errors() {
    assert!(parse_err("MATCH (n RETURN n").contains("expected ')' to close the node pattern"));
    assert!(parse_err("MATCH (a)-[r->(b) RETURN a").contains("expected ']'"));
    assert!(parse_err("MATCH (a)->(b) RETURN a").contains("unexpected '>'"));
    assert!(parse_err("MATCH n RETURN n").contains("'(' to start a node pattern"));
    assert!(parse_err("MATCH (a)-[:]->(b) RETURN a").contains("expected a relationship type"));
}
