use super::{int, parse_err, string};
use crate::ast::{CreateVectorIndex, Statement};
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
