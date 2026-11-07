# 05 — Vector Index

**Crate:** `atlantis-vector`

This chapter documents the HNSW (Hierarchical Navigable Small World) vector index: the algorithm, distance metrics, index construction, search, deletion, and parameter tuning.

## Overview

Atlantis DB uses HNSW for approximate nearest neighbor (ANN) search. HNSW builds a multi-layer graph where upper layers are sparse (for fast navigation) and the base layer is dense (for accurate search). This gives sub-10ms latency over millions of vectors with >95% recall.

```
Layer 3 (sparse)    ●─────────────●
                    │
Layer 2             ●─────●───────●
                    │     │
Layer 1             ●──●──●──●────●
                    │  │  │  │
Layer 0 (dense)     ●──●──●──●──●──●──●──●──●
                    ─────────────────────────
                    All vectors live at layer 0
```

## Distance Metrics

**File:** [`crates/atlantis-vector/src/distance.rs`](../crates/atlantis-vector/src/distance.rs)

All metrics implement the `DistanceMetric` trait:

```rust
pub trait DistanceMetric: Send + Sync {
    fn distance(&self, a: &[f32], b: &[f32]) -> f32;
    fn name(&self) -> &'static str;
}
```

| Metric | Formula | Use Case | Default |
|---|---|---|---|
| `CosineDistance` | `1 - (a·b) / (|a|·|b|)` | Normalized embeddings (most common) | Yes |
| `EuclideanDistance` | `sqrt(Σ(aᵢ - bᵢ)²)` | Spatial / geometric data | |
| `DotProductDistance` | `-Σ(aᵢ · bᵢ)` | Pre-normalized vectors, max inner product | |

**Cosine distance** clamps the similarity to `[-1, 1]` to handle floating-point errors, then returns `1 - similarity`. Identical vectors have distance 0.0, orthogonal vectors have distance 1.0, opposite vectors have distance 2.0. Zero vectors return distance 1.0 (max).

**Dot product distance** returns the negative dot product, so "lower distance = more similar" holds consistently across all metrics.

The `from_kind(DistanceMetricKind)` factory creates a boxed metric from the config enum.

## HNSW Index

**File:** [`crates/atlantis-vector/src/hnsw.rs`](../crates/atlantis-vector/src/hnsw.rs)

```rust
pub struct HnswIndex {
    params: HnswParams,
    layers: Vec<Layer>,           // one Layer per level
    vectors: VectorStore,         // compact vector storage
    entry_point: Option<NodeId>,  // top-level entry node
    max_level: usize,             // current max layer in use
    distance: Box<dyn DistanceMetric>,
    deleted: HashSet<NodeId>,     // tombstones
}
```

Each `Layer` is a `HashMap<NodeId, Vec<NodeId>>` adjacency map. The entry point is the node with the highest assigned level.

### Insertion

The insertion algorithm follows the original HNSW paper:

1. **Validate dimensions** — the vector must match the index's configured dimensionality.
2. **Store the vector** in the `VectorStore`.
3. **Assign a random level** using the formula `level = floor(-ln(uniform(0,1)) * mL)` where `mL = 1/ln(M)`. This gives an exponentially decreasing probability of higher levels.
4. **First insert**: Set as entry point, initialize adjacency at each layer up to the assigned level.
5. **Subsequent inserts**:
   - Greedy search from the top layer down to `level + 1` to find the closest entry node.
   - At each layer from `level` down to 0, search with `ef_construction` candidate pool, select `M` (or `M0` at layer 0) nearest neighbors, add bidirectional edges.
   - Prune neighbor connections if they exceed the max connection limit.
6. **Update entry point** if the new node's level exceeds the current max level.

### Search

```rust
pub fn search(&self, query: &[f32], k: usize, ef_search: Option<usize>)
    -> Result<Vec<(NodeId, f32)>, AtlantisError>
```

1. **Validate dimensions**.
2. **Greedy search** from the top layer down to layer 1, following the closest neighbor at each step.
3. **Search layer 0** with `ef_search` candidate pool (default: `params.ef_search`, but at least `k`).
4. **Filter** out deleted nodes and empty vector slots.
5. **Sort** by distance (ascending), **truncate** to `k`.
6. Return `(NodeId, distance)` pairs.

### Search with Threshold

```rust
pub fn search_with_params(&self, query: &[f32], params: &SearchParams)
    -> Result<Vec<(NodeId, f32)>, AtlantisError>
```

`SearchParams` wraps `k`, `ef_search`, and an optional `score_threshold`. When a threshold is set, results are filtered to keep only those where `1.0 - distance >= threshold` (for cosine distance, similarity = 1 - distance).

### Deletion

Deletion uses a tombstone approach:

```rust
pub fn remove(&mut self, node_id: NodeId)
```

The node ID is added to the `deleted` set. Search results filter out deleted nodes. The vector data remains in storage but is excluded from queries. Periodic compaction to physically remove deleted nodes is a future enhancement.

## VectorStore

**File:** [`crates/atlantis-vector/src/vector_store.rs`](../crates/atlantis-vector/src/vector_store.rs)

Compact storage for vectors with fixed dimensionality:

```rust
pub struct VectorStore {
    vectors: Vec<Vec<f32>>,
    dimensions: usize,
}
```

All vectors share the same dimensionality. Vectors are stored in a contiguous `Vec<Vec<f32>>` for cache-friendly access during HNSW search.

Key methods:

| Method | Description |
|---|---|
| `new(dimensions)` | Create an empty store |
| `with_capacity(dims, cap)` | Pre-allocate capacity |
| `insert(vec)` | Insert, returns sequential `NodeId` |
| `insert_at(id, vec)` | Insert at a specific ID (for restoration) |
| `get(id)` | Get vector as `&[f32]` |
| `remove(id)` | Clear a vector slot (tombstone) |
| `contains(id)` | True if non-empty vector at ID |
| `len()` | Count of active (non-empty) vectors |

## SearchParams

**File:** [`crates/atlantis-vector/src/params.rs`](../crates/atlantis-vector/src/params.rs)

```rust
pub struct SearchParams {
    pub k: usize,
    pub ef_search: Option<usize>,
    pub score_threshold: Option<f32>,
}
```

Builder-style construction:

```rust
let params = SearchParams::top_k(10)
    .with_threshold(0.7)
    .with_ef_search(200);
```

## HNSW Parameters

The index parameters control the trade-off between recall, latency, and memory:

| Parameter | Default | Description |
|---|---|---|
| `m` | 16 | Connections per node on insertion (layers > 0). Higher = better recall, more memory. |
| `m0` | 32 | Max connections at layer 0. Should be `2 * m`. |
| `ef_construction` | 200 | Candidate pool size during insertion. Higher = better index quality, slower build. |
| `ef_search` | 50 | Candidate pool size during search. Higher = better recall, slower search. |
| `max_level` | 16 | Hard cap on the number of layers. |

### Tuning Guide

- **Higher recall**: Increase `ef_search` (e.g., 100-200) and `m` (e.g., 24-48).
- **Faster search**: Decrease `ef_search` (e.g., 20-50), but recall may drop.
- **Faster ingestion**: Decrease `ef_construction` (e.g., 100), but index quality may suffer.
- **Memory-constrained**: Decrease `m` (e.g., 8-12), but recall may drop.

The defaults (M=16, ef_construction=200, ef_search=50) are based on the original HNSW paper and provide a good balance for most use cases.

## Recall

The index includes a recall test that compares HNSW results against brute-force search. With M=16, ef_construction=200, ef_search=100 over 500 32-dimensional vectors, the test verifies recall > 80%. In practice, with the default parameters and larger ef_search, recall typically exceeds 95% (matching the performance target).

## Persistence

**File:** [`crates/atlantis-vector/src/persistence.rs`](../crates/atlantis-vector/src/persistence.rs)

HNSW persistence is currently a **stub** — `serialize_index` returns an empty vector and `deserialize_index` returns an error. This is listed in the [Implementation Roadmap](13-implementation-roadmap.md) as remaining work. The HNSW graph structure (layers, adjacency) and all vectors need to be serialized to disk as part of the cold tier and loaded on startup.

In the current implementation, the HNSW index is rebuilt in memory on engine startup. Vectors must be re-inserted. Full persistence is planned.

## Concurrency Model

The HNSW index is owned by the core engine actor and accessed via the query executor. Because the actor processes one command at a time (serialized), no internal locking is needed. The `DistanceMetric` trait requires `Send + Sync` for safe sharing, but the index itself is only accessed from the single actor.

This is a deliberate design choice from the requirements: "As a single-writer actor, the HNSW index receives insert/search commands via messages; no internal locking needed."

## Design Decisions

**Why HNSW over other ANN algorithms?**
HNSW offers the best balance of recall and performance for general-purpose ANN search. It handles incremental insertions efficiently (unlike tree-based methods that require rebalancing) and supports sub-10ms search over millions of vectors. It is the algorithm used by Qdrant, FalkorDB, and pgvector.

**Why tombstone-based deletion instead of immediate removal?**
Immediate removal from a multi-layer graph structure is complex — it requires reconnecting neighbors across layers. Tombstones are simple: mark the node as deleted, filter it from search results. Periodic compaction can physically remove tombstoned nodes. This is the approach used by most HNSW implementations.

**Why `Vec<Vec<f32>>` for vector storage instead of a flat buffer?**
The current `VectorStore` uses `Vec<Vec<f32>>` for simplicity. Each vector is a contiguous `Vec<f32>` with good cache locality during distance computation. A flat `Vec<f32>` with index arithmetic would be more cache-friendly for bulk operations but adds complexity. This is a potential optimization for the future.

**Why cosine as the default distance metric?**
Most embedding models (OpenAI, sentence-transformers, etc.) produce vectors where cosine similarity is the natural metric. Cosine distance handles vectors of different magnitudes gracefully and is invariant to vector length. For pre-normalized vectors, cosine reduces to a dot product.

## What's Next

- **[06 — Cypher Query Language](06-cypher-query-language.md)** — How `db.vector.search` is parsed.
- **[07 — Query Planner & Executor](07-query-planner-executor.md)** — How the executor calls the HNSW index.
- **[11 — RAG Patterns](11-rag-patterns.md)** — Practical usage of vector search with graph traversal.
- **[12 — Configuration Reference](12-configuration.md)** — Vector and HNSW config options.
