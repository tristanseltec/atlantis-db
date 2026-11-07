//! Shared helpers for the Atlantis DB benchmarks.

use atlantis_db::{Engine, Value};
use tokio::runtime::Runtime;

/// A single-threaded runtime for driving the async API from criterion.
pub fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime")
}

/// An in-memory engine with `people` Person nodes in a ring of KNOWS edges
/// (each person knows the next one).
pub async fn social_graph(people: i64) -> Engine {
    let engine = Engine::open_in_memory().await.expect("open engine");
    engine
        .execute(
            "UNWIND range(0, $n - 1) AS i CREATE (:Person {id: i, name: 'p' + toString(i), age: i % 80})",
            &[("n", Value::Integer(people))],
        )
        .await
        .expect("create people");
    engine
        .execute(
            "UNWIND range(0, $n - 1) AS i
             MATCH (a:Person {id: i}), (b:Person {id: (i + 1) % $n})
             CREATE (a)-[:KNOWS]->(b)",
            &[("n", Value::Integer(people))],
        )
        .await
        .expect("create edges");
    engine
}

/// A deterministic pseudo-random vector (xorshift), so runs are comparable.
pub fn vector(seed: u64, dims: usize) -> Vec<f32> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..dims)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 2000) as f32 / 1000.0 - 1.0
        })
        .collect()
}
