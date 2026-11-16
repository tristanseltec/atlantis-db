//! The workloads, run in order against one adapter.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::adapter::{Adapter, Memory};
use crate::dataset::{Dataset, Rng};
use crate::truth::{recall, Truth};

#[derive(Clone, Debug)]
pub struct Settings {
    pub k: usize,
    pub threads: usize,
    pub duration: Duration,
    pub write_rate: u32,
    pub freshness_probes: usize,
}

/// Latency distribution in milliseconds.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Latency {
    pub count: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
    pub max_ms: f64,
}

impl Latency {
    pub fn from(mut samples: Vec<Duration>) -> Latency {
        if samples.is_empty() {
            return Latency::default();
        }
        samples.sort_unstable();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let at = |p: f64| {
            ms(samples[((samples.len() as f64 * p).ceil() as usize).clamp(1, samples.len()) - 1])
        };
        Latency {
            count: samples.len(),
            mean_ms: samples.iter().map(|d| ms(*d)).sum::<f64>() / samples.len() as f64,
            p50_ms: at(0.50),
            p95_ms: at(0.95),
            p99_ms: at(0.99),
            p999_ms: at(0.999),
            max_ms: ms(*samples.last().expect("non-empty")),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub latency: Latency,
    pub recall_at_k: f64,
    /// Queries that returned fewer than k rows.
    pub short_results: usize,
}

#[derive(Debug, Serialize)]
pub struct LoadResult {
    pub items: usize,
    pub load_s: f64,
    pub items_per_s: f64,
    pub index_build_s: f64,
}

#[derive(Debug, Serialize)]
pub struct ConcurrentResult {
    pub threads: usize,
    pub qps: f64,
    pub latency: Latency,
    pub errors: u64,
}

/// What the writer achieved during a mixed run.
#[derive(Debug, Default)]
pub struct WriterResult {
    pub per_s: f64,
    pub latency: Latency,
    pub errors: u64,
}

#[derive(Debug, Serialize)]
pub struct MixedResult {
    pub readers: ConcurrentResult,
    pub target_writes_per_s: u32,
    pub achieved_writes_per_s: f64,
    pub write_latency: Latency,
    pub write_errors: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Results {
    pub load: Option<LoadResult>,
    pub vector: Option<SearchResult>,
    pub filtered_vector: Option<SearchResult>,
    pub graph_rag: Option<Latency>,
    pub graph_rag_concurrent: Option<ConcurrentResult>,
    pub mixed: Option<MixedResult>,
    /// Fraction of probes where a write was visible to search in its own transaction.
    pub reads_own_writes: Option<f64>,
    pub reopen_s: Option<f64>,
}

pub fn load(a: &dyn Adapter, ds: &Dataset) -> anyhow::Result<LoadResult> {
    let s = a.load(ds)?;
    Ok(LoadResult {
        items: s.items,
        load_s: s.load.as_secs_f64(),
        items_per_s: s.items as f64 / s.load.as_secs_f64().max(1e-9),
        index_build_s: s.index_build.as_secs_f64(),
    })
}

pub fn search(
    a: &dyn Adapter,
    ds: &Dataset,
    truth: &Truth,
    k: usize,
    filtered: bool,
) -> anyhow::Result<SearchResult> {
    let mut samples = Vec::with_capacity(ds.queries.len());
    let (mut total, mut short) = (0.0, 0);
    for (i, q) in ds.queries.iter().enumerate() {
        let start = Instant::now();
        let got = a.vector_topk(q, k, filtered)?;
        samples.push(start.elapsed());
        let expected = if filtered {
            &truth.filtered[i]
        } else {
            &truth.unfiltered[i]
        };
        total += recall(&got, expected);
        if got.len() < expected.len() {
            short += 1;
        }
    }
    Ok(SearchResult {
        latency: Latency::from(samples),
        recall_at_k: total / ds.queries.len() as f64,
        short_results: short,
    })
}

pub fn graph_rag(a: &dyn Adapter, ds: &Dataset, k: usize) -> anyhow::Result<Latency> {
    let mut samples = Vec::with_capacity(ds.queries.len());
    for q in &ds.queries {
        let start = Instant::now();
        a.graph_rag(q, k)?;
        samples.push(start.elapsed());
    }
    Ok(Latency::from(samples))
}

/// Runs graph-RAG queries on `s.threads` threads for `s.duration`, with a
/// writer at `write_rate` writes per second when it is non-zero.
pub fn concurrent(
    a: &dyn Adapter,
    ds: &Dataset,
    s: &Settings,
    write_rate: u32,
    next_id: &AtomicU64,
) -> anyhow::Result<(ConcurrentResult, Option<WriterResult>)> {
    let stop = AtomicBool::new(false);
    let errors = AtomicU64::new(0);
    let started = Instant::now();
    let (reader_samples, writer) = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..s.threads)
            .map(|t| {
                let (stop, errors) = (&stop, &errors);
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    let mut i = t;
                    while !stop.load(Ordering::Relaxed) {
                        let q = &ds.queries[i % ds.queries.len()];
                        let start = Instant::now();
                        if a.graph_rag(q, s.k).is_err() {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                        samples.push(start.elapsed());
                        i += s.threads;
                    }
                    samples
                })
            })
            .collect();
        let writer = (write_rate > 0).then(|| {
            let stop = &stop;
            scope.spawn(move || {
                let mut rng = Rng::new(0x5EED_0F57_17E5);
                let interval = Duration::from_secs_f64(1.0 / write_rate as f64);
                let (mut samples, mut errs) = (Vec::new(), 0u64);
                let begin = Instant::now();
                let mut next = begin;
                while !stop.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if now < next {
                        std::thread::sleep(next - now);
                        continue;
                    }
                    next += interval;
                    let src = rng.below(ds.spec.chunks as u64) as usize;
                    let m = Memory {
                        id: next_id.fetch_add(1, Ordering::Relaxed),
                        user: rng.below(ds.spec.users as u64) as u32,
                        vector: ds.vector(src),
                        text: &ds.text[src],
                        entity: rng.below(ds.spec.entities() as u64) as u32,
                    };
                    let start = Instant::now();
                    if a.write_memory(&m).is_err() {
                        errs += 1;
                    }
                    samples.push(start.elapsed());
                }
                (samples, errs, begin.elapsed())
            })
        });
        std::thread::sleep(s.duration);
        stop.store(true, Ordering::Relaxed);
        let samples: Vec<Duration> = readers
            .into_iter()
            .flat_map(|h| h.join().expect("reader thread"))
            .collect();
        (samples, writer.map(|h| h.join().expect("writer thread")))
    });
    let elapsed = started.elapsed().as_secs_f64();
    let readers = ConcurrentResult {
        threads: s.threads,
        qps: reader_samples.len() as f64 / elapsed,
        latency: Latency::from(reader_samples),
        errors: errors.load(Ordering::Relaxed),
    };
    let writer = writer.map(|(samples, errors, took)| WriterResult {
        per_s: samples.len() as f64 / took.as_secs_f64().max(1e-9),
        latency: Latency::from(samples),
        errors,
    });
    Ok((readers, writer))
}

pub fn reads_own_writes(
    a: &dyn Adapter,
    ds: &Dataset,
    probes: usize,
    next_id: &AtomicU64,
) -> anyhow::Result<f64> {
    let mut rng = Rng::new(0xF2E5_4E55);
    let mut seen = 0;
    for _ in 0..probes {
        let src = rng.below(ds.spec.chunks as u64) as usize;
        // A vector far from the corpus (negated), so only the new write can be its top hit.
        let vector: Vec<f32> = ds.vector(src).iter().map(|x| -x).collect();
        let m = Memory {
            id: next_id.fetch_add(1, Ordering::Relaxed),
            user: ds.user[src],
            vector: &vector,
            text: &ds.text[src],
            entity: 0,
        };
        if a.reads_own_write(&m)? {
            seen += 1;
        }
    }
    Ok(seen as f64 / probes.max(1) as f64)
}
