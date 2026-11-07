//! Record store benchmarks: inserts, point lookups and traversal.

use atlantis_benches::{runtime, social_graph};
use atlantis_db::{Direction, Engine, Object, RecordKey, Value};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};

fn bench_storage(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("record_insert_100_per_tx", |b| {
        b.iter_batched(
            || rt.block_on(Engine::open_in_memory()).unwrap(),
            |engine| {
                rt.block_on(engine.transaction(|tx| {
                    for i in 0..100 {
                        let mut doc = Object::new();
                        doc.insert("n".into(), Value::Integer(i));
                        tx.create("Item", Some(RecordKey::Integer(i)), vec![], doc)?;
                    }
                    Ok(())
                }))
                .unwrap()
            },
            BatchSize::SmallInput,
        )
    });

    let engine = rt.block_on(social_graph(10_000));
    let mut i = 0i64;
    c.bench_function("record_get_by_id_10k", |b| {
        b.iter(|| {
            i = (i + 7919) % 10_000;
            let id = format!("Person:{i}").parse().unwrap();
            rt.block_on(engine.read(|tx| tx.get(&id))).unwrap().expect("exists")
        })
    });

    c.bench_function("neighbors_outgoing_10k", |b| {
        b.iter(|| {
            i = (i + 7919) % 10_000;
            let id = format!("Person:{i}").parse().unwrap();
            rt.block_on(engine.read(|tx| tx.neighbors(&id, Direction::Outgoing, Some("KNOWS")))).unwrap()
        })
    });
}

criterion_group!(benches, bench_storage);
criterion_main!(benches);
