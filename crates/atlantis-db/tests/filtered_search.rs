//! Filtered vector search: conditions in `YIELD ... WHERE` are pushed into
//! the index, so `k` means "the k nearest records that match".

use atlantis_db::{Engine, ResultSet, Value};

const DIM: usize = 8;
const DOCS: usize = 2_000;
const USERS: usize = 100;

fn vector(i: usize) -> Vec<f32> {
    (0..DIM)
        .map(|d| ((i * 31 + d * 7) as f32 * 0.137).sin())
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    (dot / (na * nb)) as f64
}

/// `DOCS` documents; doc `i` belongs to user `i % USERS`, has rank `i`.
async fn corpus() -> Engine {
    let e = Engine::open_in_memory().await.unwrap();
    e.execute(
        "CREATE VECTOR INDEX doc_vec FOR (d:Doc) ON (d.embedding) OPTIONS {dimension: 8}",
        &[],
    )
    .await
    .unwrap();
    let rows: Vec<Value> = (0..DOCS)
        .map(|i| {
            let mut m = atlantis_db::Object::new();
            m.insert("n".into(), Value::Integer(i as i64));
            m.insert("user".into(), Value::String(format!("u{}", i % USERS)));
            m.insert("v".into(), Value::Vector(vector(i)));
            Value::Object(m)
        })
        .collect();
    e.execute(
        "UNWIND $rows AS r CREATE (:Doc {n: r.n, user: r.user, rank: r.n, embedding: r.v})",
        &[("rows", Value::Array(rows))],
    )
    .await
    .unwrap();
    e
}

/// Exact top-k doc numbers among docs that satisfy `keep`.
fn truth(q: &[f32], k: usize, keep: impl Fn(usize) -> bool) -> Vec<i64> {
    let mut all: Vec<(usize, f64)> = (0..DOCS)
        .filter(|&i| keep(i))
        .map(|i| (i, cosine(q, &vector(i))))
        .collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    all.into_iter().take(k).map(|(i, _)| i as i64).collect()
}

fn ns(rs: &ResultSet) -> Vec<i64> {
    rs.rows()
        .map(|r| match r.get("n").unwrap_or(&Value::Null) {
            Value::Integer(n) => *n,
            other => panic!("n is {other:?}"),
        })
        .collect()
}

async fn search(e: &Engine, q: &str, params: &[(&str, Value)]) -> Vec<i64> {
    ns(&e.execute(q, params).await.unwrap())
}

#[tokio::test]
async fn equality_filter_returns_the_k_nearest_matches() {
    let e = corpus().await;
    for probe in [3usize, 500, 1234] {
        let q = vector(probe);
        let user = format!("u{}", probe % USERS);
        let got = search(
            &e,
            "CALL db.vector.search('doc_vec', $q, 5) YIELD node, score \
             WHERE node.user = $u RETURN node.n AS n",
            &[("q", Value::Vector(q.clone())), ("u", Value::String(user))],
        )
        .await;
        // 1% of the corpus matches; filtering after the search used to
        // return about 0.05 rows here.
        assert_eq!(got, truth(&q, 5, |i| i % USERS == probe % USERS));
    }
}

#[tokio::test]
async fn operand_order_alias_in_and_range() {
    let e = corpus().await;
    let q = vector(77);
    fn p<'a>(q: &[f32], extra: &[(&'a str, Value)]) -> Vec<(&'a str, Value)> {
        let mut v = vec![("q", Value::Vector(q.to_vec()))];
        v.extend(extra.iter().cloned());
        v
    }
    // Reversed operands and an aliased yield.
    let got = search(
        &e,
        "CALL db.vector.search('doc_vec', $q, 4) YIELD node AS d WHERE 'u7' = d.user RETURN d.n AS n",
        &p(&q, &[]),
    )
    .await;
    assert_eq!(got, truth(&q, 4, |i| i % USERS == 7));
    // IN over a list.
    let got = search(
        &e,
        "CALL db.vector.search('doc_vec', $q, 6) YIELD node WHERE node.user IN $us RETURN node.n AS n",
        &p(
            &q,
            &[(
            "us",
            Value::Array(vec![Value::String("u1".into()), Value::String("u2".into())]),
        )]),
    )
    .await;
    assert_eq!(got, truth(&q, 6, |i| i % USERS == 1 || i % USERS == 2));
    // Range, combined with equality (both pushed down).
    let got = search(
        &e,
        "CALL db.vector.search('doc_vec', $q, 3) YIELD node \
         WHERE node.rank >= 1000 AND node.user STARTS WITH 'u9' RETURN node.n AS n",
        &p(&q, &[]),
    )
    .await;
    assert_eq!(
        got,
        truth(&q, 3, |i| i >= 1000
            && format!("u{}", i % USERS).starts_with("u9"))
    );
}

#[tokio::test]
async fn conditions_that_cannot_be_pushed_still_filter() {
    let e = corpus().await;
    let q = vector(5);
    // `score` is a yielded column: evaluated after the search, as before.
    let rs = e
        .execute(
            "CALL db.vector.search('doc_vec', $q, 10) YIELD node, score \
             WHERE node.user = 'u5' AND score > 2.0 RETURN node.n AS n",
            &[("q", Value::Vector(q.clone()))],
        )
        .await
        .unwrap();
    assert_eq!(rs.rows().count(), 0);
    // A comparison between two fields of the node is not pushed down.
    let got = search(
        &e,
        "CALL db.vector.search('doc_vec', $q, 10) YIELD node \
         WHERE node.user = 'u5' AND node.n = node.rank RETURN node.n AS n",
        &[("q", Value::Vector(q.clone()))],
    )
    .await;
    assert_eq!(got, truth(&q, 10, |i| i % USERS == 5));
    // Comparing with null matches nothing.
    let got = search(
        &e,
        "CALL db.vector.search('doc_vec', $q, 10) YIELD node WHERE node.user = null RETURN node.n AS n",
        &[("q", Value::Vector(q))],
    )
    .await;
    assert!(got.is_empty());
}

#[tokio::test]
async fn filter_value_from_the_outer_row() {
    let e = corpus().await;
    let q = vector(42);
    let rs = e
        .execute(
            "UNWIND ['u3', 'u4'] AS who \
             CALL db.vector.search('doc_vec', $q, 2) YIELD node WHERE node.user = who \
             RETURN who, node.n AS n",
            &[("q", Value::Vector(q.clone()))],
        )
        .await
        .unwrap();
    let mut want = truth(&q, 2, |i| i % USERS == 3);
    want.extend(truth(&q, 2, |i| i % USERS == 4));
    assert_eq!(ns(&rs), want);
}

#[tokio::test]
async fn filtered_columns_follow_commits() {
    let e = corpus().await;
    let q = vector(9);
    let query = "CALL db.vector.search('doc_vec', $q, 3) YIELD node WHERE node.user = 'new' \
                 RETURN node.n AS n";
    let params = [("q", Value::Vector(q.clone()))];
    // Builds the `user` column.
    assert!(search(&e, query, &params).await.is_empty());
    // A field change (same vector) moves records into the filter...
    e.execute(
        "MATCH (d:Doc) WHERE d.n IN [10, 20, 30, 40] SET d.user = 'new'",
        &[],
    )
    .await
    .unwrap();
    let mut got = search(&e, query, &params).await;
    let mut want = truth(&q, 3, |i| [10, 20, 30, 40].contains(&i));
    got.sort();
    want.sort();
    assert_eq!(got, want);
    // ...a delete takes one out, and a new record comes in.
    e.execute("MATCH (d:Doc {n: 10}) DETACH DELETE d", &[])
        .await
        .unwrap();
    e.execute(
        "CREATE (:Doc {n: 9999, user: 'new', rank: 0, embedding: $v})",
        &[("v", Value::Vector(q.clone()))],
    )
    .await
    .unwrap();
    let got = search(&e, query, &params).await;
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], 9999, "the exact match ranks first");
    assert!(!got.contains(&10));
}
