//! Vector index benchmarks: index build and k-NN search through Cypher.

use atlantis_benches::{runtime, vector};
use atlantis_db::{Engine, Value};
use criterion::{criterion_group, criterion_main, Criterion};

const DIMS: usize = 128;
const DOCS: u64 = 5_000;

fn bench_vector(c: &mut Criterion) {
    let rt = runtime();
    let engine = rt.block_on(async {
        let engine = Engine::open_in_memory().await.unwrap();
        engine
            .execute(
                "CREATE VECTOR INDEX doc_embedding FOR (d:Doc) ON (d.embedding) OPTIONS {dimension: 128}",
                &[],
            )
            .await
            .unwrap();
        let docs: Vec<Value> = (0..DOCS).map(|i| Value::Vector(vector(i, DIMS))).collect();
        engine
            .execute(
                "UNWIND range(0, size($docs) - 1) AS i CREATE (:Doc {id: i, embedding: $docs[i]})",
                &[("docs", Value::Array(docs))],
            )
            .await
            .unwrap();
        // The first search builds the index; keep that out of the search timing.
        engine
            .execute("CALL db.vector.search('doc_embedding', $q, 1)", &[("q", Value::Vector(vector(1, DIMS)))])
            .await
            .unwrap();
        engine
    });

    let mut seed = 1_000_000u64;
    c.bench_function("vector_search_top10_5k_128d", |b| {
        b.iter(|| {
            seed += 1;
            rt.block_on(engine.execute(
                "CALL db.vector.search('doc_embedding', $q, 10) YIELD node, score RETURN node.id, score",
                &[("q", Value::Vector(vector(seed, DIMS)))],
            ))
            .unwrap()
        })
    });
}

criterion_group!(benches, bench_vector);
criterion_main!(benches);
