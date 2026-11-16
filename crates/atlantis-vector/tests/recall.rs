//! Recall and connectivity regression tests on clustered data.
//!
//! Uniform random vectors hide graph-construction bugs: every node has
//! neighbours everywhere, so even a fragmented graph finds good answers.
//! Clustered data with a low intrinsic dimension (like real embeddings) does
//! not: if links between clusters are lost, search stays in the wrong
//! cluster. These tests caught the connectivity bug that the Retrieval Bench
//! found (recall@10 of 0.22 at 20,000 × 768).

use std::collections::HashSet;

use atlantis_core::{DistanceMetricKind, NodeId};
use atlantis_vector::distance::{CosineDistance, DistanceMetric};
use atlantis_vector::{AllowSet, FilterStrategy, HnswIndex, HnswParams};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

const DIM: usize = 32;
const CLUSTERS: usize = 40;
const LOCAL_DIM: usize = 4;

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v
        .iter()
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt()
        .max(f32::MIN_POSITIVE);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

fn gauss(rng: &mut StdRng) -> f32 {
    // Box–Muller.
    let u1: f32 = rng.gen_range(f32::EPSILON..1.0);
    let u2: f32 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

/// `n` unit vectors from `CLUSTERS` clusters, each varying along `LOCAL_DIM`
/// directions, plus `queries` vectors drawn the same way.
fn clustered(n: usize, queries: usize, seed: u64) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let mut rng = StdRng::seed_from_u64(seed);
    let centres: Vec<Vec<f32>> = (0..CLUSTERS)
        .map(|_| unit((0..DIM).map(|_| gauss(&mut rng)).collect()))
        .collect();
    let bases: Vec<Vec<Vec<f32>>> = (0..CLUSTERS)
        .map(|_| {
            (0..LOCAL_DIM)
                .map(|_| unit((0..DIM).map(|_| gauss(&mut rng)).collect()))
                .collect()
        })
        .collect();
    let sample = |rng: &mut StdRng| {
        let c = rng.gen_range(0..CLUSTERS);
        let mut v = centres[c].clone();
        for b in &bases[c] {
            let a = 0.35 * gauss(rng);
            v.iter_mut().zip(b).for_each(|(x, y)| *x += a * y);
        }
        v.iter_mut().for_each(|x| *x += 0.02 * gauss(rng));
        unit(v)
    };
    let data = (0..n).map(|_| sample(&mut rng)).collect();
    let qs = (0..queries).map(|_| sample(&mut rng)).collect();
    (data, qs)
}

fn build(data: &[Vec<f32>]) -> HnswIndex {
    let mut index = HnswIndex::new(HnswParams::default(), DIM, DistanceMetricKind::Cosine);
    for (i, v) in data.iter().enumerate() {
        index.insert(NodeId(i as u64), v.clone()).unwrap();
    }
    index
}

fn exact(data: &[Vec<f32>], q: &[f32], k: usize, skip: &HashSet<u64>) -> Vec<u64> {
    let mut all: Vec<(u64, f32)> = data
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip.contains(&(*i as u64)))
        .map(|(i, v)| (i as u64, CosineDistance.distance(q, v)))
        .collect();
    all.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    all.into_iter().take(k).map(|(i, _)| i).collect()
}

fn recall(index: &HnswIndex, data: &[Vec<f32>], queries: &[Vec<f32>], skip: &HashSet<u64>) -> f64 {
    let k = 10;
    let mut total = 0.0;
    for q in queries {
        let truth = exact(data, q, k, skip);
        let got: HashSet<u64> = index
            .search(q, k, Some(64))
            .unwrap()
            .into_iter()
            .map(|(id, _)| id.0)
            .collect();
        total += truth.iter().filter(|t| got.contains(t)).count() as f64 / k as f64;
    }
    total / queries.len() as f64
}

#[test]
fn recall_on_clustered_data() {
    let (data, queries) = clustered(4_000, 100, 7);
    let index = build(&data);
    let r = recall(&index, &data, &queries, &HashSet::new());
    assert!(
        r >= 0.95,
        "recall@10 {r:.3} on clustered data, expected ≥ 0.95"
    );
}

#[test]
fn every_node_is_reachable() {
    let (data, _) = clustered(4_000, 0, 11);
    let index = build(&data);
    let found = data
        .iter()
        .enumerate()
        .filter(|(i, v)| {
            index
                .search(v, 1, Some(64))
                .unwrap()
                .first()
                .is_some_and(|(id, d)| id.0 == *i as u64 || *d <= 1e-6)
        })
        .count();
    let share = found as f64 / data.len() as f64;
    assert!(share >= 0.995, "only {share:.3} of nodes find themselves");
}

#[test]
fn recall_holds_after_deletes() {
    let (data, queries) = clustered(4_000, 100, 13);
    let mut index = build(&data);
    let mut rng = StdRng::seed_from_u64(99);
    let mut deleted = HashSet::new();
    while deleted.len() < data.len() * 3 / 10 {
        let id = rng.gen_range(0..data.len() as u64);
        if deleted.insert(id) {
            index.remove(NodeId(id));
        }
    }
    let r = recall(&index, &data, &queries, &deleted);
    assert!(
        r >= 0.90,
        "recall@10 {r:.3} after deleting 30%, expected ≥ 0.90"
    );
}

fn build_parallel(data: &[Vec<f32>], threads: usize) -> HnswIndex {
    let mut index = HnswIndex::new(HnswParams::default(), DIM, DistanceMetricKind::Cosine);
    let items = data
        .iter()
        .enumerate()
        .map(|(i, v)| (NodeId(i as u64), v.clone()))
        .collect();
    index.insert_batch(items, threads).unwrap();
    index
}

#[test]
fn parallel_build_keeps_recall() {
    let (data, queries) = clustered(6_000, 100, 17);
    let index = build_parallel(&data, 8);
    assert_eq!(index.len(), data.len());
    let r = recall(&index, &data, &queries, &HashSet::new());
    assert!(
        r >= 0.95,
        "recall@10 {r:.3} after a parallel build, expected ≥ 0.95"
    );
}

#[test]
fn parallel_build_reaches_every_node() {
    let (data, _) = clustered(6_000, 0, 19);
    let index = build_parallel(&data, 8);
    let found = data
        .iter()
        .enumerate()
        .filter(|(i, v)| {
            index
                .search(v, 1, Some(64))
                .unwrap()
                .first()
                .is_some_and(|(id, d)| id.0 == *i as u64 || *d <= 1e-6)
        })
        .count();
    let share = found as f64 / data.len() as f64;
    assert!(share >= 0.995, "only {share:.3} of nodes find themselves");
}

#[test]
fn parallel_batch_onto_existing_graph() {
    // A second batch links into a graph that already exists, then
    // sequential inserts keep working on top of it.
    let (data, queries) = clustered(5_000, 100, 23);
    let mut index = build_parallel(&data[..2_000], 8);
    let items = data[2_000..4_500]
        .iter()
        .enumerate()
        .map(|(i, v)| (NodeId((2_000 + i) as u64), v.clone()))
        .collect();
    index.insert_batch(items, 8).unwrap();
    for (i, v) in data.iter().enumerate().skip(4_500) {
        index.insert(NodeId(i as u64), v.clone()).unwrap();
    }
    assert_eq!(index.len(), data.len());
    let r = recall(&index, &data, &queries, &HashSet::new());
    assert!(r >= 0.95, "recall@10 {r:.3}, expected ≥ 0.95");
}

/// Mean recall@10 of a filtered search against exact filtered ground truth;
/// also checks every hit is allowed.
fn filtered_recall(
    index: &HnswIndex,
    data: &[Vec<f32>],
    queries: &[Vec<f32>],
    allow: &AllowSet,
) -> f64 {
    let k = 10;
    let skip: HashSet<u64> = (0..data.len() as u64)
        .filter(|i| !allow.contains(*i as u32))
        .collect();
    let mut total = 0.0;
    for q in queries {
        let truth = exact(data, q, k, &skip);
        let hits = index.search_filtered(q, k, Some(64), allow).unwrap();
        assert!(hits.iter().all(|(id, _)| allow.contains(id.0 as u32)));
        assert_eq!(hits.len(), truth.len().min(k), "short result");
        let got: HashSet<u64> = hits.into_iter().map(|(id, _)| id.0).collect();
        total +=
            truth.iter().filter(|t| got.contains(t)).count() as f64 / truth.len().max(1) as f64;
    }
    total / queries.len() as f64
}

fn random_allow(n: usize, share: f64, seed: u64) -> AllowSet {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n as u32).filter(|_| rng.gen_bool(share)).collect()
}

#[test]
fn filtered_search_at_one_percent() {
    let (data, queries) = clustered(10_000, 100, 29);
    let index = build_parallel(&data, 8);
    let allow = random_allow(data.len(), 0.01, 1);
    assert_eq!(
        index.filter_strategy(allow.len(), 64),
        FilterStrategy::Exact
    );
    let r = filtered_recall(&index, &data, &queries, &allow);
    assert!(r >= 0.999, "filtered recall@10 {r:.3} at 1%");
}

#[test]
fn filtered_search_at_half() {
    let (data, queries) = clustered(10_000, 100, 31);
    let index = build_parallel(&data, 8);
    let allow = random_allow(data.len(), 0.5, 2);
    assert_eq!(
        index.filter_strategy(allow.len(), 64),
        FilterStrategy::Traversal
    );
    let r = filtered_recall(&index, &data, &queries, &allow);
    assert!(r >= 0.95, "filtered recall@10 {r:.3} at 50%");
}

#[test]
fn filtered_search_with_correlated_filter() {
    // Allowed nodes sit on one side of a hyperplane, so for half the queries
    // every allowed node is far away and the walk must cross disallowed
    // regions to reach them.
    let (data, queries) = clustered(10_000, 100, 37);
    let index = build_parallel(&data, 8);
    let allow: AllowSet = (0..data.len() as u32)
        .filter(|&i| data[i as usize][0] > 0.05)
        .collect();
    let share = allow.len() as f64 / data.len() as f64;
    assert!((0.2..0.8).contains(&share), "share {share}");
    let r = filtered_recall(&index, &data, &queries, &allow);
    assert!(
        r >= 0.95,
        "filtered recall@10 {r:.3} with a correlated filter"
    );
}

#[test]
fn filtered_search_skips_deleted_and_handles_empty() {
    let (data, queries) = clustered(3_000, 20, 41);
    let mut index = build(&data);
    let allow = random_allow(data.len(), 0.3, 3);
    let victims: Vec<u32> = allow.iter().step_by(2).collect();
    for &v in &victims {
        index.remove(NodeId(v as u64));
    }
    for q in &queries {
        let hits = index.search_filtered(q, 10, None, &allow).unwrap();
        assert_eq!(hits.len(), 10);
        assert!(hits.iter().all(|(id, _)| !victims.contains(&(id.0 as u32))));
    }
    assert!(index
        .search_filtered(&queries[0], 10, None, &AllowSet::new())
        .unwrap()
        .is_empty());
}
