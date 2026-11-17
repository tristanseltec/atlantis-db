//! Property indexes, constraints, `EXPLAIN` and `SHOW` through Cypher.

use atlantis_db::{Engine, Error, Value};

type Params<'a> = Vec<(&'a str, Value)>;

async fn run(e: &Engine, q: &str, params: &[(&str, Value)]) -> Vec<String> {
    let rs = e
        .execute(q, params)
        .await
        .unwrap_or_else(|err| panic!("{q}: {err}"));
    let mut rows: Vec<String> = rs
        .rows()
        .map(|r| {
            r.values()
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect();
    rows.sort();
    rows
}

async fn ok(e: &Engine, q: &str) {
    e.execute(q, &[])
        .await
        .unwrap_or_else(|err| panic!("{q}: {err}"));
}

async fn explain(e: &Engine, q: &str, params: &[(&str, Value)]) -> String {
    let rs = e.execute(&format!("EXPLAIN {q}"), params).await.unwrap();
    rs.rows()
        .map(|r| format!("{} {}", r.values()[0], r.values()[1]))
        .collect::<Vec<_>>()
        .join("\n")
}

fn obj_x() -> atlantis_db::Object {
    let mut o = atlantis_db::Object::new();
    o.insert("x".into(), Value::Integer(1));
    o
}

/// Deterministic, messy data: mixed types, nulls, missing fields, arrays,
/// nested objects, integers and floats that compare equal, huge integers.
async fn seed(e: &Engine) {
    let mut x: u64 = 7;
    let mut next = || {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        x >> 33
    };
    let names = ["Ana", "Anh", "Bao", "Binh", "Chi", "", "an", "Ánh"];
    let cities = ["Hue", "Da Nang", "Hanoi"];
    for i in 0..240i64 {
        let r = next();
        let age = match r % 9 {
            0 => Value::Null,
            1 => Value::Float((r % 50) as f64 + 0.5),
            2 => Value::Float((r % 50) as f64),
            3 => Value::String(format!("{}", r % 50)),
            4 => Value::Integer((1 << 53) + (next() % 3) as i64),
            5 => Value::Float(-0.0),
            _ => Value::Integer((r % 50) as i64),
        };
        let tags = match r % 5 {
            0 => Value::Array(vec!["vip".into(), "beta".into(), "vip".into()]),
            1 => Value::Array(vec![Value::Integer(1), Value::Float(2.0), Value::Null]),
            2 => Value::Array(vec!["vip".into()]),
            3 => Value::Array(vec![]),
            _ => Value::Null,
        };
        let mut addr = atlantis_db::Object::new();
        if r % 4 != 0 {
            addr.insert("city".into(), cities[(r % 3) as usize].into());
        }
        let labels = if r % 6 == 0 { "Person:Admin" } else { "Person" };
        let q = format!(
            "CREATE (p:{labels} {{id: $id, seq: $id, email: $email, age: $age, tags: $tags, \
             name: $name, address: $addr, team: $team, no: $no}})"
        );
        let params: Params = vec![
            ("id", Value::Integer(i)),
            ("email", Value::String(format!("user{i}@x"))),
            ("age", age),
            ("tags", tags),
            ("name", names[(r % 8) as usize].into()),
            ("addr", Value::Object(addr)),
            ("team", Value::Integer((r % 4) as i64)),
            (
                "no",
                match next() % 9 {
                    0 => Value::Null,
                    1 => Value::Object(obj_x()),
                    2 => Value::Float(f64::NAN),
                    3 => Value::Array(vec![Value::Integer(1), Value::Null]),
                    k => Value::Integer((k % 5) as i64),
                },
            ),
        ];
        e.execute(&q, &params).await.unwrap();
    }
    ok(e, "CREATE (:Robot {age: 3, email: 'r@x'})").await;
    ok(
        e,
        "MATCH (a:Person), (b:Person) WHERE b.seq = a.seq + 1 AND a.team < 2 CREATE (a)-[:KNOWS]->(b)",
    )
    .await;
}

/// Queries whose answers must not change when indexes appear.
fn queries() -> Vec<(&'static str, Params<'static>)> {
    let big = Value::Integer((1 << 53) + 1);
    vec![
        ("MATCH (p:Person) WHERE p.age = 3 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age = 3.0 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE 3 = p.age RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age = 0 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age = $a RETURN p.id", vec![("a", big.clone())]),
        ("MATCH (p:Person) WHERE p.age = $a RETURN p.id", vec![("a", Value::Float((1u64 << 53) as f64))]),
        ("MATCH (p:Person) WHERE p.age = '3' RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age = null RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age > 40 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age >= 10 AND p.age < 12.5 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE 10 < p.age RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age > '4' RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age > $a RETURN p.id", vec![("a", big)]),
        ("MATCH (p:Person) WHERE p.age IN [1, 2.0, '3', null] RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age IN $l RETURN p.id", vec![("l", Value::Null)]),
        ("MATCH (p:Person) WHERE 'vip' IN p.tags RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE 2 IN p.tags RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.tags = 'vip' RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.tags = ['vip', 'beta', 'vip'] RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.name STARTS WITH 'An' RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.name STARTS WITH '' RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.name >= 'B' AND p.name < 'C' RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.address.city = 'Hue' RETURN p.id", vec![]),
        ("MATCH (p:Person {email: 'user7@x'}) RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.email = $e RETURN p.id", vec![("e", "user9@x".into())]),
        ("MATCH (p:Admin) WHERE p.email STARTS WITH 'user1' RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.team = 1 AND p.no = 2 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.team = 2 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.team = 1 OR p.no = 2 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.team = 1 AND p.no > 1 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.team IN [1, 3] AND p.no = 3 RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.team = 0 AND p.no = $o RETURN p.id", vec![("o", Value::Object(obj_x()))]),
        ("MATCH (p:Person) WHERE p.team = 0 AND p.no IN [$o, 2] RETURN p.id", vec![("o", Value::Object(obj_x()))]),
        ("MATCH (p:Person {team: 2}) WHERE p.no IS NOT NULL RETURN p.id", vec![]),
        ("MATCH (p:Person) WHERE p.id = 5 RETURN p.email", vec![]),
        ("UNWIND [1, 2, 3] AS t MATCH (p:Person) WHERE p.team = t AND p.age < 5 RETURN t, p.id", vec![]),
        ("MATCH (p:Person) WHERE p.age = 4 OPTIONAL MATCH (q:Person) WHERE q.email = p.email + 'nope' RETURN p.id, q", vec![]),
        ("MATCH (p:Person), (q:Person) WHERE p.team = 3 AND q.email = 'user1@x' RETURN p.id, q.id", vec![]),
        ("MATCH (p:Person), (q:Person) WHERE p.age = 7 AND q.email = p.email RETURN p.id, q.id", vec![]),
        ("MATCH (p:Person)-[:KNOWS]->(f), (q:Person) WHERE q.team = f.team AND q.age = 3 RETURN q.id", vec![]),
    ]
}

async fn answers(e: &Engine) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (q, params) in queries() {
        out.push(run(e, q, &params).await);
    }
    out
}

#[tokio::test]
async fn indexes_never_change_answers() {
    let e = Engine::open_in_memory().await.unwrap();
    seed(&e).await;
    let before = answers(&e).await;
    assert!(before.iter().filter(|rows| !rows.is_empty()).count() > 30);
    let joined = queries()
        .iter()
        .position(|(q, _)| q.contains(":KNOWS"))
        .unwrap();
    assert!(!before[joined].is_empty());
    for ddl in [
        "CREATE INDEX FOR (p:Person) ON (p.age)",
        "CREATE INDEX FOR (p:Person) ON (p.tags)",
        "CREATE INDEX FOR (p:Person) ON (p.name)",
        "CREATE INDEX FOR (p:Person) ON (p.address.city)",
        "CREATE INDEX FOR (p:Person) ON (p.team, p.no)",
        "CREATE INDEX FOR (p:Admin) ON (p.email)",
        "CREATE CONSTRAINT FOR (p:Person) REQUIRE p.email IS UNIQUE",
    ] {
        ok(&e, ddl).await;
    }
    let after = answers(&e).await;
    for (((q, _), b), a) in queries().iter().zip(&before).zip(&after) {
        assert_eq!(b, a, "{q}");
    }
    // Every query above that has an index-usable condition used one.
    let seeks = [
        "MATCH (p:Person) WHERE p.age = 3 RETURN p.id",
        "MATCH (p:Person) WHERE 'vip' IN p.tags RETURN p.id",
        "MATCH (p:Person) WHERE p.name STARTS WITH 'An' RETURN p.id",
        "MATCH (p:Person) WHERE p.address.city = 'Hue' RETURN p.id",
        "MATCH (p:Person) WHERE p.team = 1 AND p.no = 2 RETURN p.id",
        "MATCH (p:Person) WHERE p.age >= 10 AND p.age < 12.5 RETURN p.id",
    ];
    for q in seeks {
        let plan = explain(&e, q, &[]).await;
        assert!(plan.contains("NodeIndexSeek"), "{q}\n{plan}");
    }
}

#[tokio::test]
async fn explain_shows_the_chosen_access_path() {
    let e = Engine::open_in_memory().await.unwrap();
    ok(&e, "CREATE INDEX person_email FOR (p:Person) ON (p.email)").await;
    ok(
        &e,
        "CREATE CONSTRAINT person_key FOR (p:Person) REQUIRE (p.org, p.no) IS NODE KEY",
    )
    .await;
    let plan = explain(&e, "MATCH (p:Person) WHERE p.email = $e RETURN p", &[]).await;
    assert_eq!(
        plan,
        "'NodeIndexSeek' 'p:Person WHERE p.email = $e (index person_email)'\n\
         'Filter' 'p.email = $e'\n\
         'Projection' 'p'"
    );
    let plan = explain(
        &e,
        "MATCH (p:Person {org: 'acme', no: 7})-[:KNOWS]->(f) RETURN f.name ORDER BY f.name LIMIT 3",
        &[],
    )
    .await;
    assert!(plan.starts_with("'NodeUniqueIndexSeek'"), "{plan}");
    assert!(
        plan.contains("'Expand(All)' '(p:Person)-[:KNOWS]->(f)'"),
        "{plan}"
    );
    assert!(
        plan.contains("'Sort'") && plan.contains("'Limit' '3'"),
        "{plan}"
    );
    // No usable condition: a label scan.
    let plan = explain(&e, "MATCH (p:Person) WHERE p.name = 'x' RETURN p", &[]).await;
    assert!(plan.starts_with("'NodeByLabelScan' 'p:Person'"), "{plan}");
    // A composite index needs a condition on every field.
    ok(&e, "CREATE INDEX team_no FOR (p:Person) ON (p.team, p.no)").await;
    let plan = explain(&e, "MATCH (p:Person) WHERE p.team = 1 RETURN p", &[]).await;
    assert!(plan.starts_with("'NodeByLabelScan'"), "{plan}");
    let plan = explain(
        &e,
        "MATCH (p:Person) WHERE p.team = 1 AND p.no > 2 RETURN p",
        &[],
    )
    .await;
    assert!(plan.contains("(index team_no)"), "{plan}");
    // A node key has an entry for every node: any prefix works.
    let plan = explain(&e, "MATCH (p:Person) WHERE p.org = 'acme' RETURN p", &[]).await;
    assert!(plan.contains("(index person_key)"), "{plan}");
    // A later path's start node can be looked up from an earlier path's
    // values: an index nested loop join.
    ok(
        &e,
        "CREATE INDEX order_email FOR (o:Order) ON (o.customerEmail)",
    )
    .await;
    let plan = explain(
        &e,
        "MATCH (p:Person), (o:Order) WHERE o.customerEmail = p.email AND p.org = 'acme' RETURN o",
        &[],
    )
    .await;
    assert!(
        plan.contains("o:Order WHERE o.customerEmail = p.email (index order_email)"),
        "{plan}"
    );
    let plan = explain(
        &e,
        "MATCH (o:Order), (p:Person) WHERE o.customerEmail = p.email RETURN o",
        &[],
    )
    .await;
    assert!(plan.contains("'NodeByLabelScan' 'o:Order'"), "{plan}");
    let plan = explain(&e, "MATCH (n) RETURN count(n)", &[]).await;
    assert!(
        plan.starts_with("'AllNodesScan'") && plan.contains("EagerAggregation"),
        "{plan}"
    );
    let plan = explain(&e, "MATCH (p:Person) WHERE p.id = 3 RETURN p", &[]).await;
    assert!(plan.starts_with("'NodeByIdSeek'"), "{plan}");
    // EXPLAIN does not run the query.
    explain(&e, "CREATE (:Person {email: 'a'})", &[]).await;
    assert_eq!(
        run(&e, "MATCH (p:Person) RETURN count(p)", &[]).await,
        ["0"]
    );
}

#[tokio::test]
async fn unique_constraints_through_cypher() {
    let e = Engine::open_in_memory().await.unwrap();
    ok(
        &e,
        "CREATE CONSTRAINT email_uq FOR (p:Person) REQUIRE p.email IS UNIQUE",
    )
    .await;
    ok(&e, "CREATE (:Person {email: 'a@x'})").await;
    let err = e
        .execute("CREATE (:Person {email: 'a@x'})", &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)), "{err}");
    assert!(err.to_string().contains("email_uq"), "{err}");
    // The failed statement rolled back as a whole.
    assert_eq!(
        run(&e, "MATCH (p:Person) RETURN count(p)", &[]).await,
        ["1"]
    );
    let err = e
        .execute(
            "UNWIND ['b@x', 'b@x'] AS m CREATE (:Person {email: m})",
            &[],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)));
    assert_eq!(
        run(&e, "MATCH (p:Person) RETURN count(p)", &[]).await,
        ["1"]
    );
    // MERGE finds the existing node through the constraint index.
    ok(
        &e,
        "MERGE (p:Person {email: 'a@x'}) ON MATCH SET p.seen = true",
    )
    .await;
    assert_eq!(
        run(&e, "MATCH (p:Person) RETURN p.email, p.seen", &[]).await,
        ["'a@x' | true"]
    );
    // Changing a value frees the old one.
    ok(&e, "MATCH (p:Person {email: 'a@x'}) SET p.email = 'c@x'").await;
    ok(&e, "CREATE (:Person {email: 'a@x'})").await;
    // Creating a constraint over duplicates fails and leaves nothing behind.
    ok(&e, "CREATE (:Person {nick: 'x'}), (:Person {nick: 'x'})").await;
    let err = e
        .execute(
            "CREATE CONSTRAINT nick_uq FOR (p:Person) REQUIRE p.nick IS UNIQUE",
            &[],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)), "{err}");
    assert_eq!(run(&e, "SHOW CONSTRAINTS", &[]).await.len(), 1);
}

#[tokio::test]
async fn not_null_and_node_key_through_cypher() {
    let e = Engine::open_in_memory().await.unwrap();
    ok(
        &e,
        "CREATE CONSTRAINT FOR (p:Person) REQUIRE p.name IS NOT NULL",
    )
    .await;
    ok(
        &e,
        "CREATE CONSTRAINT FOR (b:Book) REQUIRE (b.isbn, b.edition) IS NODE KEY",
    )
    .await;
    ok(&e, "CREATE (:Person {name: 'Ana'})").await;
    for q in [
        "CREATE (:Person {age: 3})",
        "MATCH (p:Person) REMOVE p.name",
        "MATCH (p:Person) SET p.name = null",
        "MATCH (p:Person) SET p = {age: 4}",
        "CREATE (:Book {isbn: '1'})",
        "CREATE (:Robot) WITH 1 AS x CREATE (:Book {edition: 1})",
    ] {
        let err = e.execute(q, &[]).await.unwrap_err();
        assert!(matches!(err, Error::ConstraintViolation(_)), "{q}: {err}");
    }
    ok(
        &e,
        "CREATE (:Book {isbn: '1', edition: 1}), (:Book {isbn: '1', edition: 2})",
    )
    .await;
    let err = e
        .execute("CREATE (:Book {isbn: '1', edition: 1.0})", &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)), "{err}");
    // A secondary label brings a node under the constraint.
    ok(&e, "CREATE (:Robot {id: 'r1'})").await;
    let err = e
        .execute("MATCH (r:Robot {id: 'r1'}) SET r:Person", &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)), "{err}");
    assert_eq!(
        run(&e, "MATCH (p:Person) RETURN count(p)", &[]).await,
        ["1"]
    );
}

#[tokio::test]
async fn ddl_names_if_not_exists_drop_and_show() {
    let e = Engine::open_in_memory().await.unwrap();
    ok(&e, "CREATE INDEX FOR (p:Person) ON (p.address.city)").await;
    ok(
        &e,
        "CREATE INDEX IF NOT EXISTS FOR (p:Person) ON (p.address.city)",
    )
    .await;
    ok(
        &e,
        "CREATE INDEX other IF NOT EXISTS FOR (p:Person) ON (p.address.city)",
    )
    .await;
    ok(
        &e,
        "CREATE CONSTRAINT uq IF NOT EXISTS FOR (p:Person) REQUIRE p.email IS UNIQUE",
    )
    .await;
    ok(
        &e,
        "CREATE CONSTRAINT uq IF NOT EXISTS FOR (p:Person) REQUIRE p.email IS UNIQUE",
    )
    .await;
    ok(
        &e,
        "CREATE VECTOR INDEX emb FOR (p:Person) ON (p.v) OPTIONS {dimension: 2}",
    )
    .await;
    assert_eq!(
        run(&e, "SHOW INDEXES", &[]).await,
        [
            "'emb' | 'VECTOR' | ['Person'] | ['v'] | null",
            "'index_Person_address_city' | 'RANGE' | ['Person'] | ['address.city'] | null",
            "'uq' | 'RANGE' | ['Person'] | ['email'] | 'uq'",
        ]
    );
    assert_eq!(
        run(&e, "SHOW CONSTRAINTS", &[]).await,
        ["'uq' | 'UNIQUENESS' | ['Person'] | ['email']"]
    );
    for (q, msg) in [
        ("CREATE INDEX uq FOR (p:Person) ON (p.x)", "already exists"),
        (
            "CREATE INDEX FOR (p:Person) ON (p.address.city)",
            "already covers",
        ),
        ("DROP INDEX uq", "is a constraint"),
        ("DROP CONSTRAINT index_Person_address_city", "is an index"),
        ("DROP INDEX nope", "does not exist"),
        ("DROP CONSTRAINT nope", "does not exist"),
        ("CREATE INDEX FOR (p:Person) ON (p.id)", "record key"),
    ] {
        let err = e.execute(q, &[]).await.unwrap_err().to_string();
        assert!(err.contains(msg), "{q}: {err}");
    }
    ok(&e, "DROP INDEX nope IF EXISTS").await;
    ok(&e, "DROP INDEX index_Person_address_city").await;
    ok(&e, "DROP CONSTRAINT uq").await;
    ok(&e, "DROP INDEX emb").await;
    assert!(run(&e, "SHOW INDEXES", &[]).await.is_empty());
}

#[tokio::test]
async fn indexes_and_constraints_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let e = Engine::open(dir.path()).await.unwrap();
        ok(
            &e,
            "CREATE CONSTRAINT uq FOR (p:Person) REQUIRE p.email IS UNIQUE",
        )
        .await;
        ok(&e, "CREATE INDEX by_age FOR (p:Person) ON (p.age)").await;
        ok(
            &e,
            "CREATE (:Person {email: 'a@x', age: 30}), (:Person {email: 'b@x', age: 31})",
        )
        .await;
        e.checkpoint().await.unwrap();
        ok(&e, "CREATE (:Person {email: 'c@x', age: 30})").await;
        e.shutdown().await.unwrap();
    }
    let e = Engine::open(dir.path()).await.unwrap();
    assert_eq!(
        run(&e, "MATCH (p:Person) WHERE p.age = 30 RETURN p.email", &[]).await,
        ["'a@x'", "'c@x'"]
    );
    assert!(
        explain(&e, "MATCH (p:Person) WHERE p.age = 30 RETURN p", &[])
            .await
            .contains("by_age")
    );
    let err = e
        .execute("CREATE (:Person {email: 'c@x'})", &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)), "{err}");
}

#[tokio::test]
async fn concurrent_merges_on_a_unique_key_create_one_node() {
    let e = std::sync::Arc::new(Engine::open_in_memory().await.unwrap());
    ok(
        &e,
        "CREATE CONSTRAINT uq FOR (u:User) REQUIRE u.name IS UNIQUE",
    )
    .await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let e = e.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                match e
                    .execute("MERGE (u:User {name: 'ana'}) RETURN u", &[])
                    .await
                {
                    Ok(_) => return,
                    Err(Error::TxConflict(_)) => continue,
                    Err(other) => panic!("{other}"),
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(run(&e, "MATCH (u:User) RETURN count(u)", &[]).await, ["1"]);
}
