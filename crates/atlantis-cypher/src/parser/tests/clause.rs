use super::{bin, int, parse_err, prop, query, string, var};
use crate::ast::{
    BinaryOp, Clause, Expression, OrderItem, Projection, RemoveItem, SetItem, YieldItem,
};

fn clauses(src: &str) -> Vec<Clause> {
    query(src).clauses
}

fn one_clause(src: &str) -> Clause {
    let mut c = clauses(src);
    assert_eq!(c.len(), 1, "{src}");
    c.remove(0)
}

fn ret(src: &str) -> Projection {
    query(src).ret.expect("RETURN")
}

#[test]
fn match_and_optional_match_with_where() {
    let c = clauses("MATCH (n:P) WHERE n.age > 3 OPTIONAL MATCH (n)-->(m) RETURN n, m");
    let Clause::Match {
        optional: false,
        where_clause: Some(w),
        ..
    } = &c[0]
    else {
        panic!("{:?}", c[0])
    };
    assert_eq!(*w, bin(prop(var("n"), "age"), BinaryOp::Gt, int(3)));
    assert!(matches!(
        &c[1],
        Clause::Match {
            optional: true,
            where_clause: None,
            ..
        }
    ));
    assert!(matches!(
        one_clause("optional match (n) where n:A"),
        Clause::Match {
            optional: true,
            where_clause: Some(_),
            ..
        }
    ));
}

#[test]
fn create_clause() {
    let Clause::Create(p) = one_clause("CREATE (a:A {x: 1})-[:R]->(b:B), (c)") else {
        panic!()
    };
    assert_eq!(p.paths.len(), 2);
    assert_eq!(p.paths[0].steps.len(), 1);
}

#[test]
fn merge_with_on_create_and_on_match_in_any_order() {
    let Clause::Merge { path, on_create, on_match } =
        one_clause("MERGE (n:P {id: 1}) ON MATCH SET n.seen = true ON CREATE SET n.c = 1, n.d = 2 ON MATCH SET n:Old")
    else {
        panic!()
    };
    assert_eq!(path.start.labels, vec!["P"]);
    assert_eq!(on_create.len(), 2);
    assert_eq!(on_match.len(), 2);
    assert!(matches!(&on_match[1], SetItem::Labels { labels, .. } if labels == &["Old"]));
    assert!(
        parse_err("MERGE (n) ON DELETE SET n.x = 1").contains("expected CREATE or MATCH after ON")
    );
}

#[test]
fn set_items() {
    let Clause::Set(items) = one_clause("SET n.p = v, n.a.b = 1, n = {x: 1}, n += $props, n:L1:L2")
    else {
        panic!()
    };
    assert_eq!(
        items,
        vec![
            SetItem::Property {
                variable: "n".into(),
                path: vec!["p".into()],
                value: var("v")
            },
            SetItem::Property {
                variable: "n".into(),
                path: vec!["a".into(), "b".into()],
                value: int(1)
            },
            SetItem::Replace {
                variable: "n".into(),
                value: Expression::Map(vec![("x".into(), int(1))])
            },
            SetItem::Merge {
                variable: "n".into(),
                value: Expression::Parameter("props".into())
            },
            SetItem::Labels {
                variable: "n".into(),
                labels: vec!["L1".into(), "L2".into()]
            },
        ]
    );
    assert!(parse_err("SET n").contains("expected '.', '=', '+=' or ':'"));
    assert!(parse_err("SET n.p 1").contains("expected '='"));
}

#[test]
fn set_property_then_return() {
    let q = query("MATCH (n) SET n.p = v RETURN n");
    assert_eq!(q.clauses.len(), 2);
    assert!(matches!(&q.clauses[1], Clause::Set(items) if items.len() == 1));
    assert_eq!(q.ret.unwrap().items[0].text, "n");
}

#[test]
fn remove_items() {
    let Clause::Remove(items) = one_clause("REMOVE n.a.b, n:L1:L2") else {
        panic!()
    };
    assert_eq!(
        items,
        vec![
            RemoveItem::Property {
                variable: "n".into(),
                path: vec!["a".into(), "b".into()]
            },
            RemoveItem::Labels {
                variable: "n".into(),
                labels: vec!["L1".into(), "L2".into()]
            },
        ]
    );
    assert!(parse_err("REMOVE n").contains("expected '.' or ':'"));
}

#[test]
fn delete_and_detach_delete() {
    assert_eq!(
        one_clause("DELETE a, r"),
        Clause::Delete {
            detach: false,
            exprs: vec![var("a"), var("r")]
        }
    );
    assert_eq!(
        one_clause("detach delete n"),
        Clause::Delete {
            detach: true,
            exprs: vec![var("n")]
        }
    );
    assert!(parse_err("DETACH n").contains("expected DELETE"));
}

#[test]
fn with_clause() {
    let Clause::With {
        projection,
        where_clause,
    } = one_clause(
        "WITH DISTINCT n.x AS x, count(*) AS c ORDER BY c DESC LIMIT 5 SKIP 1 WHERE c > 1",
    )
    else {
        panic!()
    };
    assert!(projection.distinct && !projection.star);
    assert_eq!(projection.items.len(), 2);
    assert_eq!(projection.items[1].expr, Expression::CountStar);
    assert_eq!(
        projection.order_by,
        vec![OrderItem {
            expr: var("c"),
            descending: true
        }]
    );
    assert_eq!(projection.skip, Some(int(1)));
    assert_eq!(projection.limit, Some(int(5)));
    assert_eq!(where_clause, Some(bin(var("c"), BinaryOp::Gt, int(1))));

    let Clause::With { projection, .. } = one_clause("WITH *, n.x AS y") else {
        panic!()
    };
    assert!(projection.star);
    assert_eq!(projection.items[0].alias.as_deref(), Some("y"));
}

#[test]
fn unwind_clause() {
    assert_eq!(
        one_clause("UNWIND [1, 2] AS x"),
        Clause::Unwind {
            expr: Expression::List(vec![int(1), int(2)]),
            variable: "x".into()
        }
    );
    assert!(parse_err("UNWIND [1] x").contains("expected AS"));
}

#[test]
fn call_clause() {
    assert_eq!(
        one_clause("CALL db.labels"),
        Clause::Call {
            procedure: "db.labels".into(),
            args: vec![],
            yields: None,
            where_clause: None
        }
    );
    assert_eq!(
        one_clause("CALL db.vector.search('idx', $v, 10) YIELD node AS n, score WHERE score > 0.5"),
        Clause::Call {
            procedure: "db.vector.search".into(),
            args: vec![string("idx"), Expression::Parameter("v".into()), int(10)],
            yields: Some(vec![
                YieldItem {
                    column: "node".into(),
                    alias: Some("n".into())
                },
                YieldItem {
                    column: "score".into(),
                    alias: None
                },
            ]),
            where_clause: Some(bin(
                var("score"),
                BinaryOp::Gt,
                Expression::Literal(atlantis_core::Value::Float(0.5))
            )),
        }
    );
    assert!(matches!(
        one_clause("CALL db.labels() YIELD *"),
        Clause::Call { yields: None, .. }
    ));
}

#[test]
fn return_projection() {
    let p = ret("MATCH (n) RETURN DISTINCT n.name, n.age + 1 AS next ORDER BY n.name ASCENDING, next DESCENDING SKIP 2 LIMIT 10");
    assert!(p.distinct);
    assert_eq!(p.items[0].text, "n.name");
    assert_eq!(p.items[0].name(), "n.name");
    assert_eq!(p.items[1].text, "n.age + 1");
    assert_eq!(p.items[1].name(), "next");
    assert_eq!(
        p.order_by.iter().map(|o| o.descending).collect::<Vec<_>>(),
        vec![false, true]
    );
    assert_eq!((p.skip, p.limit), (Some(int(2)), Some(int(10))));

    let p = ret("RETURN *");
    assert!(p.star && p.items.is_empty());
    let p = ret("RETURN count( DISTINCT  x )  ,  toUpper(n.`odd key`) AS ASC");
    assert_eq!(p.items[0].text, "count( DISTINCT  x )");
    assert_eq!(p.items[1].text, "toUpper(n.`odd key`)");
    assert_eq!(p.items[1].alias.as_deref(), Some("ASC"));
    assert!(!ret("RETURN n ORDER BY n.x asc").order_by[0].descending);
    assert_eq!(
        ret("RETURN n AS order").items[0].alias.as_deref(),
        Some("order")
    );
}

#[test]
fn trailing_text_after_return_is_an_error() {
    let e = parse_err("MATCH (n) RETURN n garbage");
    assert!(
        e.contains("1:20") && e.contains("'garbage'") && e.contains("after RETURN"),
        "{e}"
    );
    assert!(parse_err("RETURN n MATCH (m)").contains("after RETURN"));
    assert!(parse_err("RETURN 1 LIMIT 1 LIMIT 2").contains("at most one SKIP and one LIMIT"));
    assert!(parse_err("RETURN n ORDER n").contains("expected BY"));
}

#[test]
fn clauses_require_known_keywords() {
    assert!(parse_err("MATCH (n) FOO").contains("expected a clause, RETURN, UNION"));
    assert!(parse_err("FOO (n)").contains("expected a clause such as MATCH"));
    assert!(parse_err("RETURN").contains("expected an expression"));
}

#[test]
fn keywords_as_names_where_unambiguous() {
    let q = query(
        "MATCH (o:Order)-[:KEY]->(i:INDEX) WHERE o.type = 'x' AND o.order > 1 RETURN o.key AS type",
    );
    let Clause::Match { pattern, .. } = &q.clauses[0] else {
        panic!()
    };
    assert_eq!(pattern.paths[0].start.labels, vec!["Order"]);
    assert_eq!(pattern.paths[0].steps[0].0.types, vec!["KEY"]);
    assert_eq!(pattern.paths[0].steps[0].1.labels, vec!["INDEX"]);
    assert_eq!(q.ret.unwrap().items[0].name(), "type");
}
