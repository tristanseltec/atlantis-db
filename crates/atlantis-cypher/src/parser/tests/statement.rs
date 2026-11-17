use super::{int, parse_err, string};
use crate::ast::{ConstraintRequirement, CreateIndex, CreateVectorIndex, Statement};
use crate::parse;

#[test]
fn create_vector_index() {
    assert_eq!(
        parse("CREATE VECTOR INDEX doc_emb IF NOT EXISTS FOR (d:Doc) ON (d.embedding) OPTIONS {dimension: 3, metric: 'cosine'}")
            .unwrap(),
        Statement::CreateVectorIndex(CreateVectorIndex {
            name: "doc_emb".into(),
            if_not_exists: true,
            label: "Doc".into(),
            property: "embedding".into(),
            options: vec![("dimension".into(), int(3)), ("metric".into(), string("cosine"))],
        })
    );
    assert_eq!(
        parse("create vector index `my idx` for (n:Order) on (n.type);").unwrap(),
        Statement::CreateVectorIndex(CreateVectorIndex {
            name: "my idx".into(),
            if_not_exists: false,
            label: "Order".into(),
            property: "type".into(),
            options: vec![],
        })
    );
}

#[test]
fn create_vector_index_errors() {
    assert!(
        parse_err("CREATE VECTOR INDEX IF NOT EXISTS FOR (n:A) ON (n.p)")
            .contains("expected an index name")
    );
    assert!(parse_err("CREATE VECTOR INDEX i FOR (n:A) ON (m.p)")
        .contains("expected variable 'n', found 'm'"));
    assert!(parse_err("CREATE VECTOR INDEX i FOR (n) ON (n.p)").contains("expected ':'"));
    assert!(parse_err("CREATE VECTOR INDEX i FOR (n:A) ON (n.p) extra")
        .contains("expected end of input"));
}

#[test]
fn drop_index() {
    assert_eq!(
        parse("DROP INDEX idx").unwrap(),
        Statement::DropIndex {
            name: "idx".into(),
            if_exists: false
        }
    );
    assert_eq!(
        parse("drop index idx if exists").unwrap(),
        Statement::DropIndex {
            name: "idx".into(),
            if_exists: true
        }
    );
    assert!(parse_err("DROP TABLE t").contains("expected INDEX"));
}

#[test]
fn transaction_control() {
    assert_eq!(parse("BEGIN").unwrap(), Statement::Begin);
    assert_eq!(parse("commit").unwrap(), Statement::Commit);
    assert_eq!(parse("Rollback;").unwrap(), Statement::Rollback);
    assert!(parse_err("BEGIN MATCH (n)").contains("expected end of input"));
}

#[test]
fn union_and_union_all() {
    let Statement::Query(u) =
        parse("MATCH (a) RETURN a.x AS x UNION ALL MATCH (b) RETURN b.x AS x UNION RETURN 1 AS x")
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(u.first.clauses.len(), 1);
    assert_eq!(
        u.rest.iter().map(|(all, _)| *all).collect::<Vec<_>>(),
        vec![true, false]
    );
    assert!(u.rest[1].1.clauses.is_empty());
    assert!(parse_err("RETURN 1 UNION").contains("expected a clause such as MATCH"));
}

#[test]
fn update_only_queries() {
    let Statement::Query(u) = parse("MATCH (n) DETACH DELETE n").unwrap() else {
        panic!()
    };
    assert_eq!(u.first.clauses.len(), 2);
    assert!(u.first.ret.is_none());
}

fn props(paths: &[&str]) -> Vec<Vec<String>> {
    paths
        .iter()
        .map(|p| p.split('.').map(String::from).collect())
        .collect()
}

#[test]
fn create_index() {
    assert_eq!(
        parse("CREATE INDEX person_email FOR (p:Person) ON (p.email)").unwrap(),
        Statement::CreateIndex(CreateIndex {
            name: Some("person_email".into()),
            if_not_exists: false,
            label: "Person".into(),
            properties: props(&["email"]),
        })
    );
    assert_eq!(
        parse("create range index if not exists for (p:Person) on (p.last, p.address.city)")
            .unwrap(),
        Statement::CreateIndex(CreateIndex {
            name: None,
            if_not_exists: true,
            label: "Person".into(),
            properties: props(&["last", "address.city"]),
        })
    );
    assert_eq!(
        parse("CREATE INDEX `for` IF NOT EXISTS FOR (p:P) ON (p.x)").unwrap(),
        Statement::CreateIndex(CreateIndex {
            name: Some("for".into()),
            if_not_exists: true,
            label: "P".into(),
            properties: props(&["x"]),
        })
    );
}

#[test]
fn create_index_errors() {
    assert!(
        parse_err("CREATE INDEX i FOR (p:P) ON (q.x)").contains("expected variable 'p', found 'q'")
    );
    assert!(parse_err("CREATE TEXT INDEX i FOR (p:P) ON (p.x)")
        .contains("TEXT indexes are not supported yet"));
    assert!(parse_err("CREATE INDEX i FOR ()-[r:R]-() ON (r.x)")
        .contains("relationship indexes and constraints are not supported yet"));
    assert!(parse_err("CREATE INDEX i FOR (p:A:B) ON (p.x)").contains("covers one label"));
    assert!(parse_err("CREATE INDEX i FOR (p:P) ON ()").contains("expected a variable"));
}

#[test]
fn create_constraint() {
    let c = |src: &str| match parse(src).unwrap() {
        Statement::CreateConstraint(c) => c,
        other => panic!("{src}: {other:?}"),
    };
    let uq = c("CREATE CONSTRAINT email_uq FOR (p:Person) REQUIRE p.email IS UNIQUE");
    assert_eq!(uq.name.as_deref(), Some("email_uq"));
    assert_eq!(uq.requirement, ConstraintRequirement::Unique);
    assert_eq!(uq.properties, props(&["email"]));
    let node_uq = c("CREATE CONSTRAINT FOR (p:Person) REQUIRE (p.a, p.b.c) IS NODE UNIQUE");
    assert_eq!(node_uq.name, None);
    assert_eq!(node_uq.properties, props(&["a", "b.c"]));
    assert_eq!(node_uq.requirement, ConstraintRequirement::Unique);
    let nn = c("create constraint nn if not exists for (p:Person) require p.name is not null");
    assert!(nn.if_not_exists);
    assert_eq!(nn.requirement, ConstraintRequirement::NotNull);
    for src in [
        "CREATE CONSTRAINT k FOR (p:P) REQUIRE (p.a, p.b) IS NODE KEY",
        "CREATE CONSTRAINT k FOR (p:P) REQUIRE (p.a, p.b) IS KEY",
    ] {
        assert_eq!(c(src).requirement, ConstraintRequirement::NodeKey);
    }
}

#[test]
fn create_constraint_errors() {
    assert!(
        parse_err("CREATE CONSTRAINT c FOR (p:P) REQUIRE (p.a, p.b) IS NOT NULL")
            .contains("IS NOT NULL takes exactly one property")
    );
    assert!(
        parse_err("CREATE CONSTRAINT c FOR (p:P) REQUIRE p.a IS NODE")
            .contains("expected UNIQUE or KEY")
    );
    assert!(
        parse_err("CREATE CONSTRAINT c FOR (p:P) REQUIRE p.a IS SPECIAL")
            .contains("expected UNIQUE, NOT NULL, NODE KEY or KEY")
    );
    assert!(parse_err("CREATE CONSTRAINT c FOR (p:P) p.a IS UNIQUE").contains("expected REQUIRE"));
}

#[test]
fn drop_and_show() {
    assert_eq!(
        parse("DROP CONSTRAINT c IF EXISTS").unwrap(),
        Statement::DropConstraint {
            name: "c".into(),
            if_exists: true
        }
    );
    assert_eq!(parse("SHOW INDEXES").unwrap(), Statement::ShowIndexes);
    assert_eq!(parse("show all index").unwrap(), Statement::ShowIndexes);
    assert_eq!(
        parse("SHOW CONSTRAINTS;").unwrap(),
        Statement::ShowConstraints
    );
    assert!(parse_err("SHOW TABLES").contains("expected INDEXES or CONSTRAINTS"));
}

#[test]
fn explain() {
    let Statement::Explain(u) = parse("EXPLAIN MATCH (p:Person) WHERE p.x = 1 RETURN p").unwrap()
    else {
        panic!()
    };
    assert_eq!(u.first.clauses.len(), 1);
    assert!(parse_err("EXPLAIN CREATE INDEX i FOR (p:P) ON (p.x)").contains("expected"));
}

#[test]
fn create_pattern_is_not_ddl() {
    // A node pattern after CREATE is still a CREATE clause.
    assert!(matches!(
        parse("CREATE (:INDEX {x: 1})").unwrap(),
        Statement::Query(_)
    ));
}
