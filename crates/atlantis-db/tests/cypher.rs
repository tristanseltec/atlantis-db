//! Cypher on the record store, end to end through the public API.
//!
//! Many tests here pin down bugs of the previous engine (listed as Phase 0
//! in the implementation roadmap) so they stay fixed.

use atlantis_db::{Direction, Engine, Error, Object, RecordId, ResultSet, Value};

async fn engine() -> Engine {
    Engine::open_in_memory().await.unwrap()
}

async fn run(engine: &Engine, q: &str) -> ResultSet {
    engine
        .execute(q, &[])
        .await
        .unwrap_or_else(|e| panic!("{q}\n  failed: {e}"))
}

fn rows(rs: &ResultSet) -> Vec<Vec<Value>> {
    rs.rows().map(|r| r.values().to_vec()).collect()
}

fn col(rs: &ResultSet, name: &str) -> Vec<Value> {
    rs.rows()
        .map(|r| r.get(name).cloned().unwrap_or(Value::Null))
        .collect()
}

fn s(v: &str) -> Value {
    Value::from(v)
}

fn i(v: i64) -> Value {
    Value::Integer(v)
}

async fn social(engine: &Engine) {
    run(
        engine,
        "CREATE (a:Person {id: 'alice', name: 'Alice', age: 30}),
                (b:Person {id: 'bob', name: 'Bob', age: 25}),
                (c:Person {id: 'carol', name: 'Carol', age: 35}),
                (d:Company {id: 'acme', name: 'Acme'}),
                (a)-[:KNOWS {since: 2020}]->(b),
                (b)-[:KNOWS {since: 2021}]->(c),
                (a)-[:WORKS_AT]->(d)",
    )
    .await;
}

#[tokio::test]
async fn create_relationships_and_return_every_created_row() {
    let e = engine().await;
    let rs = run(
        &e,
        "UNWIND [1, 2, 3] AS x CREATE (n:Item {x: x}) RETURN n.x AS x ORDER BY x",
    )
    .await;
    assert_eq!(
        col(&rs, "x"),
        vec![i(1), i(2), i(3)],
        "CREATE returns one row per input row"
    );

    social(&e).await;
    let rs = run(
        &e,
        "MATCH (:Person)-[r:KNOWS]->(:Person) RETURN count(r) AS n",
    )
    .await;
    assert_eq!(col(&rs, "n"), vec![i(2)]);
}

#[tokio::test]
async fn multi_hop_match_binds_neighbours_not_every_node() {
    let e = engine().await;
    social(&e).await;
    let rs = run(
        &e,
        "MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person) RETURN b.name AS name",
    )
    .await;
    assert_eq!(col(&rs, "name"), vec![s("Bob")]);
    let rs = run(
        &e,
        "MATCH (a {name: 'Alice'})-[:KNOWS]->()-[:KNOWS]->(c) RETURN c.name AS name",
    )
    .await;
    assert_eq!(col(&rs, "name"), vec![s("Carol")]);
}

#[tokio::test]
async fn directions_and_untyped_relationships() {
    let e = engine().await;
    social(&e).await;
    let incoming = run(
        &e,
        "MATCH (b {name: 'Bob'})<-[:KNOWS]-(a) RETURN a.name AS name",
    )
    .await;
    assert_eq!(col(&incoming, "name"), vec![s("Alice")]);
    let both = run(
        &e,
        "MATCH (b {name: 'Bob'})-[:KNOWS]-(x) RETURN x.name AS name ORDER BY name",
    )
    .await;
    assert_eq!(col(&both, "name"), vec![s("Alice"), s("Carol")]);
    let any_type = run(
        &e,
        "MATCH (a {name: 'Alice'})-->(x) RETURN x.name AS name ORDER BY name",
    )
    .await;
    assert_eq!(col(&any_type, "name"), vec![s("Acme"), s("Bob")]);
    let types = run(
        &e,
        "MATCH (a {name: 'Alice'})-[r:KNOWS|WORKS_AT]->() RETURN type(r) AS t ORDER BY t",
    )
    .await;
    assert_eq!(col(&types, "t"), vec![s("KNOWS"), s("WORKS_AT")]);
}

#[tokio::test]
async fn variable_length_paths_and_path_functions() {
    let e = engine().await;
    social(&e).await;
    let rs = run(
        &e,
        "MATCH (a {name: 'Alice'})-[:KNOWS*1..2]->(x) RETURN x.name AS name ORDER BY name",
    )
    .await;
    assert_eq!(col(&rs, "name"), vec![s("Bob"), s("Carol")]);
    let rs = run(
        &e,
        "MATCH p = (a {name: 'Alice'})-[:KNOWS*2]->(c)
         RETURN length(p) AS hops, [n IN nodes(p) | n.name] AS names",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![vec![
            i(2),
            Value::Array(vec![s("Alice"), s("Bob"), s("Carol")])
        ]]
    );
}

#[tokio::test]
async fn set_remove_and_labels() {
    let e = engine().await;
    social(&e).await;
    run(
        &e,
        "MATCH (p:Person {name: 'Bob'}) SET p.age = 26, p.address.city = 'Hue', p:Admin",
    )
    .await;
    let rs = run(
        &e,
        "MATCH (p:Admin) RETURN p.age AS age, p.address.city AS city, labels(p) AS labels",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![vec![
            i(26),
            s("Hue"),
            Value::Array(vec![s("Person"), s("Admin")])
        ]]
    );
    run(
        &e,
        "MATCH (p:Person {name: 'Bob'}) REMOVE p.age, p:Admin SET p += {nick: 'B'}",
    )
    .await;
    let rs = run(
        &e,
        "MATCH (p {name: 'Bob'}) RETURN p.age AS age, p.nick AS nick, p:Admin AS admin",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![vec![Value::Null, s("B"), Value::Bool(false)]]
    );
}

#[tokio::test]
async fn delete_and_detach_delete() {
    let e = engine().await;
    social(&e).await;
    let err = e
        .execute("MATCH (p {name: 'Bob'}) DELETE p", &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SchemaViolation(_)), "{err}");
    run(&e, "MATCH (p {name: 'Bob'}) DETACH DELETE p").await;
    let rs = run(&e, "MATCH (p:Person) RETURN count(*) AS n").await;
    assert_eq!(col(&rs, "n"), vec![i(2)]);
    let rs = run(&e, "MATCH ()-[r:KNOWS]->() RETURN count(r) AS n").await;
    assert_eq!(col(&rs, "n"), vec![i(0)], "edges are removed with the node");
}

#[tokio::test]
async fn merge_matches_or_creates() {
    let e = engine().await;
    for _ in 0..2 {
        run(
            &e,
            "MERGE (p:Person {email: 'a@x.io'})
             ON CREATE SET p.created = true, p.visits = 1
             ON MATCH SET p.visits = p.visits + 1",
        )
        .await;
    }
    let rs = run(
        &e,
        "MATCH (p:Person) RETURN count(*) AS n, max(p.visits) AS visits",
    )
    .await;
    assert_eq!(rows(&rs), vec![vec![i(1), i(2)]]);
}

#[tokio::test]
async fn optional_match_fills_nulls() {
    let e = engine().await;
    social(&e).await;
    let rs = run(
        &e,
        "MATCH (p:Person) OPTIONAL MATCH (p)-[:WORKS_AT]->(c:Company)
         RETURN p.name AS name, c.name AS company ORDER BY name",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![
            vec![s("Alice"), s("Acme")],
            vec![s("Bob"), Value::Null],
            vec![s("Carol"), Value::Null]
        ]
    );
}

#[tokio::test]
async fn operators_and_functions() {
    let e = engine().await;
    let rs = run(
        &e,
        "RETURN 1 <> 2 AS ne, 2 IN [1, 2] AS isin, null IS NULL AS isnull, 7 % 3 AS m,
                'Atlantis' STARTS WITH 'At' AS sw, 'Atlantis' CONTAINS 'lan' AS c,
                'abc' =~ 'a.c' AS re, toUpper('x') AS up, size([1, 2, 3]) AS sz,
                CASE WHEN 1 > 2 THEN 'a' ELSE 'b' END AS cs, {a: 1}.a AS mapget,
                [x IN range(1, 5) WHERE x % 2 = 1 | x * 10] AS odds, true XOR false AS x",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            i(1),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            s("X"),
            i(3),
            s("b"),
            i(1),
            Value::Array(vec![i(10), i(30), i(50)]),
            Value::Bool(true),
        ]]
    );
    let rs = run(&e, "UNWIND [1, 2] AS x RETURN x").await;
    assert_eq!(
        col(&rs, "x"),
        vec![i(1), i(2)],
        "UNWIND as the first clause produces rows"
    );
}

#[tokio::test]
async fn aggregation_distinct_order_skip_limit() {
    let e = engine().await;
    run(
        &e,
        "UNWIND [['a', 1], ['b', 2], ['a', 3], ['c', 4], ['b', 5]] AS pair
         CREATE (:Sale {region: pair[0], amount: pair[1]})",
    )
    .await;
    let rs = run(
        &e,
        "MATCH (s:Sale) RETURN s.region AS region, sum(s.amount) AS total, count(*) AS n
         ORDER BY total DESC, region SKIP 1 LIMIT 2",
    )
    .await;
    // Totals: b = 7, a = 4, c = 4 (ties broken by region); SKIP 1 drops b.
    assert_eq!(
        rows(&rs),
        vec![vec![s("a"), i(4), i(2)], vec![s("c"), i(4), i(1)]]
    );
    let rs = run(
        &e,
        "MATCH (s:Sale) RETURN DISTINCT s.region AS r ORDER BY r",
    )
    .await;
    assert_eq!(col(&rs, "r"), vec![s("a"), s("b"), s("c")]);
    let rs = run(&e, "MATCH (s:Sale) WITH s.region AS r, collect(s.amount) AS xs WHERE size(xs) > 1 RETURN r ORDER BY r").await;
    assert_eq!(col(&rs, "r"), vec![s("a"), s("b")]);
    let rs = run(&e, "MATCH (s:Sale) RETURN avg(s.amount) AS avg, min(s.amount) AS lo, count(DISTINCT s.region) AS regions").await;
    assert_eq!(rows(&rs), vec![vec![Value::Float(3.0), i(1), i(3)]]);
    let rs = run(
        &e,
        "MATCH (s:Nothing) RETURN count(*) AS n, collect(s) AS xs",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![vec![i(0), Value::Array(vec![])]],
        "aggregates over no rows"
    );
}

#[tokio::test]
async fn union_and_parameters() {
    let e = engine().await;
    let rs = e
        .execute(
            "RETURN $a AS v UNION RETURN $a AS v UNION ALL RETURN 2 AS v",
            &[("a", i(1))],
        )
        .await
        .unwrap();
    assert_eq!(col(&rs, "v"), vec![i(1), i(2)]);
    let err = e.execute("RETURN $missing AS v", &[]).await.unwrap_err();
    assert!(err.to_string().contains("missing parameter"), "{err}");
}

#[tokio::test]
async fn return_node_gives_its_document() {
    let e = engine().await;
    run(&e, "CREATE (:Person {id: 'alice', name: 'Alice'})").await;
    let rs = run(&e, "MATCH (p:Person) RETURN p, id(p) AS id").await;
    let Value::Object(doc) = &rows(&rs)[0][0] else {
        panic!("expected a document")
    };
    let alice: RecordId = "Person:alice".parse().unwrap();
    assert_eq!(doc.get("name"), Some(&s("Alice")));
    assert_eq!(doc.get("id"), Some(&Value::RecordId(alice.clone())));
    assert_eq!(rows(&rs)[0][1], Value::RecordId(alice));
}

#[tokio::test]
async fn cypher_and_record_api_share_data() {
    let e = engine().await;
    run(
        &e,
        "CREATE (:Person {id: 'alice', name: 'Alice'})-[:KNOWS]->(:Person {id: 'bob'})",
    )
    .await;
    let friends = e
        .read(|tx| tx.neighbors(&"Person:alice".parse()?, Direction::Outgoing, Some("KNOWS")))
        .await
        .unwrap();
    assert_eq!(friends, vec!["Person:bob".parse::<RecordId>().unwrap()]);

    e.transaction(|tx| {
        let mut doc = Object::new();
        doc.insert("id".into(), s("carol"));
        let carol = tx.create_node(&["Person"], doc)?;
        tx.relate(&"Person:bob".parse()?, "KNOWS", &carol.id, Object::new())?;
        // Cypher inside the same transaction sees its uncommitted writes.
        // `n.id` is the record id, like `id(n)`.
        let rs = tx.execute(
            "MATCH (:Person {id: 'bob'})-[:KNOWS]->(c) RETURN c.id AS id",
            &[],
        )?;
        assert_eq!(
            col(&rs, "id"),
            vec![Value::RecordId("Person:carol".parse()?)]
        );
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn record_links_dereference_on_property_access() {
    let e = engine().await;
    run(&e, "CREATE (:Customer {id: 'c1', name: 'Ann'})").await;
    e.execute(
        "CREATE (:Order {id: 1, customer: rid('Customer', 'c1'), total: 9.5})",
        &[],
    )
    .await
    .unwrap();
    let rs = run(
        &e,
        "MATCH (o:Order) RETURN o.customer.name AS name, o.customer AS link",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![vec![
            s("Ann"),
            Value::RecordId("Customer:c1".parse().unwrap())
        ]]
    );
}

#[tokio::test]
async fn scripts_and_explicit_transactions() {
    let e = engine().await;
    run(&e, "CREATE (:T {v: 1}); CREATE (:T {v: 2});").await;
    let rs = run(&e, "MATCH (t:T) RETURN count(*) AS n").await;
    assert_eq!(col(&rs, "n"), vec![i(2)]);
    run(&e, "BEGIN; CREATE (:T {v: 3}); ROLLBACK;").await;
    let err = e
        .execute("BEGIN; CREATE (:T {v: 4});", &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("without COMMIT"), "{err}");
    run(&e, "BEGIN; CREATE (:T {v: 5}); CREATE (:T {v: 6}); COMMIT;").await;
    let rs = run(&e, "MATCH (t:T) RETURN collect(t.v) AS vs").await;
    let Value::Array(mut vs) = col(&rs, "vs").remove(0) else {
        panic!()
    };
    vs.sort_by_key(|v| v.as_int());
    assert_eq!(vs, vec![i(1), i(2), i(5), i(6)]);
}

#[tokio::test]
async fn trailing_text_after_return_is_an_error() {
    let e = engine().await;
    let err = e.execute("RETURN 1 AS x garbage", &[]).await.unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err}");
}

#[tokio::test]
async fn vector_index_search() {
    let e = engine().await;
    run(&e, "CREATE VECTOR INDEX doc_embedding FOR (d:Doc) ON (d.embedding) OPTIONS {dimension: 3, metric: 'cosine'}").await;
    for (name, v) in [
        ("x", [1.0, 0.0, 0.0]),
        ("y", [0.0, 1.0, 0.0]),
        ("xy", [0.7, 0.7, 0.0]),
    ] {
        e.execute(
            "CREATE (:Doc {name: $name, embedding: $v})",
            &[("name", s(name)), ("v", Value::Vector(v.to_vec()))],
        )
        .await
        .unwrap();
    }
    let query = [("q", Value::Vector(vec![1.0, 0.1, 0.0]))];
    let rs = e
        .execute(
            "CALL db.vector.search('doc_embedding', $q, 2) YIELD node, score RETURN node.name AS name, score",
            &query,
        )
        .await
        .unwrap();
    assert_eq!(col(&rs, "name"), vec![s("x"), s("xy")]);

    // The index follows later writes, including deletes.
    run(&e, "MATCH (d:Doc {name: 'x'}) DETACH DELETE d").await;
    let rs = e
        .execute(
            "CALL db.vector.search('doc_embedding', $q, 1) YIELD node RETURN node.name AS name",
            &query,
        )
        .await
        .unwrap();
    assert_eq!(col(&rs, "name"), vec![s("xy")]);

    // Standalone CALL returns its yielded columns; the label also works.
    let rs = e
        .execute("CALL db.vector.search('Doc', $q, 5, 0.99)", &query)
        .await
        .unwrap();
    assert_eq!(rs.columns(), &["node".to_string(), "score".to_string()]);
    assert_eq!(rs.row_count(), 0, "threshold filters weak matches");

    let rs = run(
        &e,
        "CALL db.indexes() YIELD name, dimension RETURN name, dimension",
    )
    .await;
    assert_eq!(rows(&rs), vec![vec![s("doc_embedding"), i(3)]]);
    run(&e, "DROP INDEX doc_embedding").await;
    assert!(e
        .execute("CALL db.vector.search('doc_embedding', $q, 1)", &query)
        .await
        .is_err());
}

#[tokio::test]
async fn data_and_vector_index_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let e = Engine::open_with_config(
            atlantis_db::Config::builder()
                .disk_path(dir.path())
                .checkpoint_interval(0)
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
        run(
            &e,
            "CREATE VECTOR INDEX v FOR (d:Doc) ON (d.e) OPTIONS {dimension: 2}",
        )
        .await;
        e.execute(
            "CREATE (:Doc {id: 1, e: $v})-[:NEXT]->(:Doc {id: 2, e: $w})",
            &[
                ("v", Value::Vector(vec![1.0, 0.0])),
                ("w", Value::Vector(vec![0.0, 1.0])),
            ],
        )
        .await
        .unwrap();
        assert!(dir.path().join("records.wal").exists());
    }
    let e = Engine::open(dir.path()).await.unwrap();
    let doc =
        |k: i64| Value::RecordId(RecordId::new("Doc", atlantis_db::RecordKey::Integer(k)).unwrap());
    let rs = run(&e, "MATCH (a:Doc)-[:NEXT]->(b) RETURN a.id AS a, b.id AS b").await;
    assert_eq!(rows(&rs), vec![vec![doc(1), doc(2)]]);
    let rs = e
        .execute(
            "CALL db.vector.search('v', $q, 1) YIELD node RETURN id(node) AS id",
            &[("q", Value::Vector(vec![0.1, 1.0]))],
        )
        .await
        .unwrap();
    assert_eq!(
        col(&rs, "id"),
        vec![doc(2)],
        "the vector index is rebuilt from records"
    );
}

#[tokio::test]
async fn delete_node_and_its_relationships_in_one_clause() {
    let e = engine().await;
    run(&e, "CREATE (a:A)-[:R]->(:B), (a)-[:R]->(:B)").await;
    run(&e, "MATCH (a:A)-[r:R]->() DELETE a, r").await;
    let rs = run(&e, "MATCH (a:A) RETURN count(*) AS n").await;
    assert_eq!(col(&rs, "n"), vec![i(0)]);
    // Order of operands does not matter either.
    run(&e, "CREATE (a:A)-[:R]->(:B), (a)-[:R]->(:B)").await;
    run(&e, "MATCH (a:A)-[r:R]->() DELETE r, a").await;
    let rs = run(&e, "MATCH ()-[r:R]->() RETURN count(r) AS n").await;
    assert_eq!(col(&rs, "n"), vec![i(0)]);
}

#[tokio::test]
async fn range_near_integer_limits_does_not_overflow() {
    let e = engine().await;
    let rs = run(
        &e,
        "RETURN range(9223372036854775800, 9223372036854775807, 5) AS r",
    )
    .await;
    assert_eq!(
        col(&rs, "r"),
        vec![Value::Array(vec![
            i(9223372036854775800),
            i(9223372036854775805)
        ])]
    );
}

#[tokio::test]
async fn a_failed_statement_poisons_its_transaction() {
    let e = engine().await;
    let result = e
        .transaction(|tx| {
            let _ignored = tx.execute("UNWIND [1, 2, 0] AS x CREATE (:N {v: 10 / x})", &[]);
            Ok(())
        })
        .await;
    assert!(
        result.is_err(),
        "commit must refuse a transaction with a failed statement"
    );
    let rs = run(&e, "MATCH (n:N) RETURN count(*) AS n").await;
    assert_eq!(
        col(&rs, "n"),
        vec![i(0)],
        "partial writes of the failed statement are discarded"
    );
}

#[tokio::test]
async fn return_star_with_aggregate_groups_by_variables() {
    let e = engine().await;
    run(&e, "CREATE (:P {x: 1}), (:P {x: 2}), (:P {x: 2})").await;
    let rs = run(
        &e,
        "MATCH (n:P) WITH n.x AS x RETURN *, count(*) AS c ORDER BY x",
    )
    .await;
    assert_eq!(rows(&rs), vec![vec![i(1), i(1)], vec![i(2), i(2)]]);
}

#[tokio::test]
async fn id_lookup_works_through_secondary_labels_and_skips_edges() {
    let e = engine().await;
    run(
        &e,
        "CREATE (:Person:Admin {id: 5})-[:KNOWS {id: 1}]->(:Person {id: 6})",
    )
    .await;
    for q in [
        "MATCH (n:Admin {id: 5}) RETURN count(*) AS n",
        "MATCH (n:Admin:Person {id: 5}) RETURN count(*) AS n",
        "MATCH (n:Person {id: 5}) RETURN count(*) AS n",
    ] {
        assert_eq!(col(&run(&e, q).await, "n"), vec![i(1)], "{q}");
    }
    let rs = run(&e, "MATCH (n:KNOWS {id: 1}) RETURN count(*) AS n").await;
    assert_eq!(
        col(&rs, "n"),
        vec![i(0)],
        "relationships are not matched as nodes"
    );
}

#[tokio::test]
async fn vector_index_keeps_recall_under_unrelated_writes() {
    let e = engine().await;
    run(
        &e,
        "CREATE VECTOR INDEX c FOR (c:C) ON (c.v) OPTIONS {dimension: 2}",
    )
    .await;
    run(
        &e,
        "UNWIND range(1, 200) AS i CREATE (:C {i: i, v: [toFloat(i), 1.0]})",
    )
    .await;
    let query = [("q", Value::Vector(vec![1.0, 1.0]))];
    let search = "CALL db.vector.search('c', $q, 100) YIELD node RETURN count(*) AS n";
    assert_eq!(
        col(&e.execute(search, &query).await.unwrap(), "n"),
        vec![i(100)]
    );
    for _ in 0..5 {
        run(&e, "MATCH (c:C) SET c.touched = true").await;
    }
    assert_eq!(
        col(&e.execute(search, &query).await.unwrap(), "n"),
        vec![i(100)]
    );
}

#[tokio::test]
async fn merge_rejects_null_properties() {
    let e = engine().await;
    let err = e
        .execute("MERGE (n:Q {name: null})", &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("null"), "{err}");
}

#[tokio::test]
async fn null_semantics_in_nested_equality_and_distinct() {
    let e = engine().await;
    let rs = run(
        &e,
        "RETURN [1, null] = [1, null] AS a, [1, 2] = [1, 3] AS b, {a: 1} = {a: 1} AS c",
    )
    .await;
    assert_eq!(
        rows(&rs),
        vec![vec![Value::Null, Value::Bool(false), Value::Bool(true)]]
    );
    let rs = run(&e, "UNWIND [{a: 1}, {a: 1.0}] AS m RETURN DISTINCT m").await;
    assert_eq!(rs.row_count(), 1);
}
