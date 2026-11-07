//! Cypher query benchmarks over a 10k-node graph.

use atlantis_benches::{runtime, social_graph};
use atlantis_db::Value;
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_query(c: &mut Criterion) {
    let rt = runtime();
    let engine = rt.block_on(social_graph(10_000));
    let mut i = 0i64;

    c.bench_function("cypher_match_by_id", |b| {
        b.iter(|| {
            i = (i + 7919) % 10_000;
            rt.block_on(engine.execute("MATCH (p:Person {id: $id}) RETURN p.name", &[("id", Value::Integer(i))]))
                .unwrap()
        })
    });

    c.bench_function("cypher_two_hop_traversal", |b| {
        b.iter(|| {
            i = (i + 7919) % 10_000;
            rt.block_on(engine.execute(
                "MATCH (p:Person {id: $id})-[:KNOWS]->()-[:KNOWS]->(f) RETURN f.name",
                &[("id", Value::Integer(i))],
            ))
            .unwrap()
        })
    });

    c.bench_function("cypher_scan_filter_aggregate_10k", |b| {
        b.iter(|| {
            rt.block_on(engine.execute(
                "MATCH (p:Person) WHERE p.age >= 40 RETURN p.age % 10 AS bucket, count(*) AS n ORDER BY bucket",
                &[],
            ))
            .unwrap()
        })
    });
}

criterion_group!(benches, bench_query);
criterion_main!(benches);
