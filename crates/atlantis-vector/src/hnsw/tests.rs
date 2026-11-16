use super::*;
use crate::distance::{CosineDistance, DistanceMetric};
use crate::params::SearchParams;
use atlantis_core::{AtlantisError, DistanceMetricKind, NodeId};
use std::collections::HashSet;

fn make_index(dims: usize) -> HnswIndex {
    HnswIndex::new(
        crate::params::HnswParams {
            m: 8,
            m0: 16,
            ef_construction: 50,
            ef_search: 20,
            max_level: 5,
        },
        dims,
        DistanceMetricKind::Cosine,
    )
}

fn random_vector(dims: usize, seed: f32) -> Vec<f32> {
    (0..dims).map(|i| (seed * (i as f32 + 1.0)).sin()).collect()
}

#[test]
fn test_insert_single() {
    let mut index = make_index(4);
    index.insert(NodeId(0), vec![1.0, 0.0, 0.0, 0.0]).unwrap();

    assert_eq!(index.len(), 1);
    assert!(index.entry_point().is_some());
    assert_eq!(index.entry_point(), Some(NodeId(0)));
}

#[test]
fn test_search_empty_index() {
    let index = make_index(4);
    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5, None).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_insert_and_search() {
    let mut index = make_index(8);

    // Insert 100 vectors. Seeds start at 0.1: seed 0 would produce the
    // all-zero vector, which has no direction and ties with everything.
    for i in 0..100 {
        let vec = random_vector(8, (i + 1) as f32 * 0.1);
        index.insert(NodeId(i), vec).unwrap();
    }

    // Search for the first inserted vector itself
    let query = random_vector(8, 0.1);
    let results = index.search(&query, 5, None).unwrap();

    assert!(!results.is_empty());
    assert!(results.len() <= 5);
    // The first result should be close to the query (same seed)
    assert_eq!(results[0].0, NodeId(0));
}

#[test]
fn test_dimension_mismatch_on_insert() {
    let mut index = make_index(4);
    let result = index.insert(NodeId(0), vec![1.0, 2.0]);
    assert!(matches!(
        result,
        Err(AtlantisError::DimensionMismatch { .. })
    ));
}

#[test]
fn test_dimension_mismatch_on_search() {
    let index = make_index(4);
    index.search(&[1.0, 2.0], 5, None).unwrap_err();
}

#[test]
fn test_remove() {
    let mut index = make_index(4);
    index.insert(NodeId(0), vec![1.0, 0.0, 0.0, 0.0]).unwrap();
    index.insert(NodeId(1), vec![0.0, 1.0, 0.0, 0.0]).unwrap();

    assert_eq!(index.len(), 2);
    index.remove(NodeId(0));
    assert_eq!(index.len(), 1);

    // Removed node should not appear in search results
    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5, None).unwrap();
    assert!(!results.iter().any(|(id, _)| *id == NodeId(0)));
}

#[test]
fn test_recall() {
    // Test that HNSW recall is reasonable (at least 80% for this small dataset)
    let mut index = HnswIndex::new(
        crate::params::HnswParams {
            m: 16,
            m0: 32,
            ef_construction: 200,
            ef_search: 100,
            max_level: 16,
        },
        32,
        DistanceMetricKind::Cosine,
    );

    // Insert 500 random vectors
    let vectors: Vec<Vec<f32>> = (0..500)
        .map(|i| random_vector(32, i as f32 * 0.01))
        .collect();
    for (i, vec) in vectors.iter().enumerate() {
        index.insert(NodeId(i as u64), vec.clone()).unwrap();
    }

    // For each query, compare HNSW results with brute-force
    let mut total_recall = 0.0;
    let num_queries = 20;

    for q_idx in 0..num_queries {
        let query = &vectors[q_idx * 10];

        // Brute force top-5
        let mut brute: Vec<(NodeId, f32)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (NodeId(i as u64), CosineDistance.distance(query, v)))
            .collect();
        brute.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let brute_top5: HashSet<NodeId> = brute.iter().take(5).map(|(id, _)| *id).collect();

        // HNSW top-5
        let hnsw_results = index.search(query, 5, None).unwrap();
        let hnsw_top5: HashSet<NodeId> = hnsw_results.iter().map(|(id, _)| *id).collect();

        let overlap = hnsw_top5.intersection(&brute_top5).count();
        total_recall += overlap as f32 / 5.0;
    }

    let avg_recall = total_recall / num_queries as f32;
    assert!(
        avg_recall > 0.80,
        "recall too low: {avg_recall:.2} (expected > 0.80)"
    );
}

#[test]
fn test_search_with_params() {
    let mut index = make_index(4);

    for i in 0..10 {
        let vec = vec![i as f32, 0.0, 0.0, 0.0];
        index.insert(NodeId(i), vec).unwrap();
    }

    let params = SearchParams::top_k(3).with_threshold(0.5);
    let results = index
        .search_with_params(&[1.0, 0.0, 0.0, 0.0], &params)
        .unwrap();
    // With threshold 0.5, only very similar results are kept
    // cosine similarity of [1,0,0,0] and [1,0,0,0] = 1.0, distance = 0.0
    // threshold 0.5 means 1 - dist >= 0.5, so dist <= 0.5
    assert!(!results.is_empty());
}

#[test]
fn test_reinsert_same_id_is_rejected() {
    let mut index = make_index(4);
    index.insert(NodeId(0), vec![1.0, 0.0, 0.0, 0.0]).unwrap();
    let err = index
        .insert(NodeId(0), vec![0.0, 1.0, 0.0, 0.0])
        .unwrap_err();
    assert!(matches!(err, AtlantisError::InvalidValue(_)));
    assert_eq!(
        index.get_vector(NodeId(0)),
        Some([1.0, 0.0, 0.0, 0.0].as_ref())
    );
}

#[test]
fn test_links_are_capped() {
    let mut index = make_index(8);
    for i in 0..300 {
        index
            .insert(NodeId(i), random_vector(8, (i + 1) as f32 * 0.07))
            .unwrap();
    }
    for node in 0..300u32 {
        for l in 0..=index.levels[node as usize] as usize {
            let cap = if l == 0 {
                index.params.m0
            } else {
                index.params.m
            };
            let links = index.link_ids(node, l);
            assert!(
                links.len() <= cap,
                "node {node} has {} links at layer {l}",
                links.len()
            );
            assert!(!links.contains(&node), "node {node} links to itself");
        }
    }
}

#[test]
fn test_deleted_entry_point_still_leads_to_results() {
    // Whatever level each node draws, deleting the entry point must not
    // make the remaining nodes unreachable.
    for _ in 0..200 {
        let mut index = make_index(3);
        index.insert(NodeId(0), vec![1.0, 0.0, 0.0]).unwrap();
        index.insert(NodeId(1), vec![0.0, 1.0, 0.0]).unwrap();
        index.insert(NodeId(2), vec![0.7, 0.7, 0.0]).unwrap();
        let entry = index.entry_point().unwrap();
        index.remove(entry);
        let results = index.search(&[1.0, 0.1, 0.0], 1, None).unwrap();
        assert_eq!(results.len(), 1, "entry {entry:?} deleted, nothing found");
        assert_ne!(results[0].0, entry);
    }
}

#[test]
fn test_get_vector() {
    let mut index = make_index(3);
    index.insert(NodeId(5), vec![1.0, 2.0, 3.0]).unwrap();

    assert_eq!(index.get_vector(NodeId(5)), Some([1.0, 2.0, 3.0].as_ref()));
    assert_eq!(index.get_vector(NodeId(99)), None);

    index.remove(NodeId(5));
    assert_eq!(index.get_vector(NodeId(5)), None);
}
