use super::*;
use crate::distance::{DistanceMetric, EuclideanDistance};
use atlantis_core::{DistanceMetricKind, NodeId};

fn params() -> crate::params::HnswParams {
    crate::params::HnswParams {
        m: 8,
        m0: 16,
        ef_construction: 64,
        ef_search: 64,
        max_level: 5,
    }
}

#[test]
fn euclidean_reports_true_distances() {
    let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Euclidean);
    for i in 0..50u64 {
        index.insert(NodeId(i), vec![i as f32, 0.0]).unwrap();
    }
    let hits = index.search(&[3.0, 4.0], 3, None).unwrap();
    assert_eq!(hits[0].0, NodeId(3));
    assert!((hits[0].1 - 4.0).abs() < 1e-5, "got {}", hits[0].1);
    assert!((hits[0].1 - EuclideanDistance.distance(&[3.0, 4.0], &[3.0, 0.0])).abs() < 1e-5);
}

#[test]
fn dot_product_prefers_larger_projection() {
    let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::DotProduct);
    for i in 1..20u64 {
        index.insert(NodeId(i), vec![i as f32, 1.0]).unwrap();
    }
    let hits = index.search(&[1.0, 0.0], 1, Some(64)).unwrap();
    assert_eq!(hits[0].0, NodeId(19));
    assert!((hits[0].1 + 19.0).abs() < 1e-5);
}

#[test]
fn rejected_batch_inserts_nothing() {
    let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Cosine);
    index.insert(NodeId(0), vec![1.0, 0.0]).unwrap();
    // Duplicate inside the batch.
    let dup = vec![(NodeId(1), vec![0.0, 1.0]), (NodeId(1), vec![1.0, 1.0])];
    index.insert_batch(dup, 4).unwrap_err();
    // Id already present.
    let present = vec![(NodeId(2), vec![0.0, 1.0]), (NodeId(0), vec![1.0, 1.0])];
    index.insert_batch(present, 4).unwrap_err();
    // Wrong dimension in the last item.
    let short = vec![(NodeId(3), vec![0.0, 1.0]), (NodeId(4), vec![1.0])];
    index.insert_batch(short, 4).unwrap_err();
    assert_eq!(index.len(), 1);
    assert!(index.get_vector(NodeId(1)).is_none());
    assert!(index.get_vector(NodeId(3)).is_none());
}

#[test]
fn small_batch_into_empty_index() {
    let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Euclidean);
    index.insert_batch(Vec::new(), 4).unwrap();
    assert!(index.entry_point().is_none());
    let items = (0..10u64)
        .map(|i| (NodeId(i), vec![i as f32, 0.0]))
        .collect();
    index.insert_batch(items, 4).unwrap();
    assert_eq!(index.len(), 10);
    assert_eq!(index.search(&[7.2, 0.0], 1, None).unwrap()[0].0, NodeId(7));
}

#[test]
fn rejects_ids_beyond_u32() {
    let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Cosine);
    index
        .insert(NodeId(u32::MAX as u64), vec![1.0, 0.0])
        .unwrap_err();
}

#[test]
fn sparse_ids_and_concurrent_searches() {
    let mut index = HnswIndex::new(params(), 4, DistanceMetricKind::Cosine);
    for i in 0..200u64 {
        let v: Vec<f32> = (0..4).map(|d| ((i * 4 + d) as f32 * 0.37).sin()).collect();
        index.insert(NodeId(i * 3), v).unwrap();
    }
    assert_eq!(index.len(), 200);
    let index = std::sync::Arc::new(index);
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let index = index.clone();
            std::thread::spawn(move || {
                for i in 0..50u64 {
                    let id = (t * 50 + i) * 3;
                    let v = index.get_vector(NodeId(id)).unwrap().to_vec();
                    let hits = index.search(&v, 1, Some(64)).unwrap();
                    assert_eq!(hits[0].0, NodeId(id));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}
