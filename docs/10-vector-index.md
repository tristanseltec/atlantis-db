# 10 — Vector Index

> **Status:** Partially implemented — HNSW, `CREATE VECTOR INDEX` / `DROP INDEX`, named indexes derived from records, and `db.vector.search` are implemented; KV persistence of the graph, index-aware filtering and quantization Planned — Phase 3
> **Crates:** `atlantis-vector`, `atlantis-engine`, `atlantis-cypher`, `atlantis-storage`

This chapter documents the HNSW (Hierarchical Navigable Small World) vector index:
the algorithm, the vector index DDL and search procedure as they work today, and the
target design for persistence, filtering, quantization and hybrid search.

## Overview

Atlantis DB uses HNSW for approximate nearest neighbor (ANN) search. HNSW builds a
multi-layer graph where upper layers are sparse (for fast navigation) and the base
layer is dense (for accurate search). Search cost grows roughly logarithmically with
the number of vectors.

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

## Implemented Today

### Distance Metrics

**File:** [`crates/atlantis-vector/src/distance.rs`](../crates/atlantis-vector/src/distance.rs)

All metrics implement the `DistanceMetric` trait (`distance(a, b) -> f32`, `name()`),
and "lower distance = more similar" holds for every metric.

| Metric | `name()` | Formula | Use case | Default |
|---|---|---|---|---|
| `CosineDistance` | `cosine` | `1 - (a·b) / (|a|·|b|)` | Normalized text embeddings | Yes |
| `EuclideanDistance` | `euclidean` | `sqrt(Σ(aᵢ - bᵢ)²)` | Geometric / unnormalized data | |
| `DotProductDistance` | `dot_product` | `-Σ(aᵢ · bᵢ)` | Pre-normalized vectors, maximum inner product | |

Cosine distance clamps the similarity to `[-1, 1]` and returns `1 - similarity`:
identical vectors give 0.0, orthogonal 1.0, opposite 2.0. A zero vector gives 1.0.
`distance::from_kind(DistanceMetricKind)` builds a boxed metric from the config enum.

The kernels under them, `distance::dot` and `distance::l2_squared`, use AVX2 and FMA on
x86-64 when the CPU has them (detected at run time) and otherwise a portable loop with
sixteen independent accumulators that the compiler vectorises. `HnswIndex` does not go
through the boxed trait: it takes a `DistanceMetricKind`, stores `1 / ‖v‖` for every
vector at insert, so a cosine distance costs one dot product, and keeps Euclidean
distances squared internally (the square root is taken only on results).

### HNSW Index

**File:** [`crates/atlantis-vector/src/hnsw.rs`](../crates/atlantis-vector/src/hnsw.rs)

`HnswIndex` is laid out for the search loop. Node ids are dense slots (below
`u32::MAX`; the engine assigns them sequentially). Vectors live in one contiguous `f32`
arena (`VectorStore`, slot `i` at `i * dim`). Layer-0 links, which every search walks,
are one flat `u32` array with a fixed stride of `m0 + 1` per node (a count, then the
ids); upper-layer links are per-node lists. Links are **directed**. Per slot it also
keeps the inverse norm, the node's level and a tombstone flag, plus the entry point
(the node with the highest level) and the current maximum level. Searches mark visited
nodes in an epoch-stamped array reused per thread, and prefetch the next neighbour's
vector. It follows Malkov & Yashunin (2018), Algorithms 1, 2 and 4.

**Insertion** — `insert(node_id, vector)`. The caller supplies the ID.

1. Validate dimensions against the index (`AtlantisError::DimensionMismatch`). An ID
   already in the index is rejected (`AtlantisError::InvalidValue`); the engine always
   inserts a changed vector under a new slot.
2. Store the vector at `node_id` in the `VectorStore`.
3. Draw a level `floor(-ln(uniform(0,1)) * mL)` with `mL = 1/ln(m)`, capped at `max_level`.
4. The first insert becomes the entry point.
5. Otherwise, descend greedily (a layer search with `ef = 1` toward the new vector) from
   the top layer to `level + 1`. At each layer from `level` down to 0, search with an
   `ef_construction` pool and link to `m` neighbours chosen by the **diversity
   heuristic**: a candidate is kept only if it is closer to the new node than to every
   neighbour already kept, and free slots are filled with the closest discarded
   candidates. Each chosen neighbour gets a reverse link; when its list exceeds its cap
   (`m0` at layer 0, `m` above), that list alone is shrunk with the same heuristic.
   Links *to* the neighbour are never removed.
6. If the new level exceeds the current maximum, the node becomes the entry point.

**Search** — `search(query, k, ef_search: Option<usize>)` returns `(NodeId, distance)`
pairs sorted ascending. It descends greedily to layer 1, searches layer 0 best-first with
`ef = ef_search.unwrap_or(params.ef_search).max(k)` (a min-heap of candidates and a
max-heap of results), and truncates to `k`.

**Threshold search** — `search_with_params(query, &SearchParams)` runs `search` and, when
`score_threshold` is set, keeps results where `1.0 - distance >= threshold`. The engine
does not use it: `db.vector.search` applies its threshold to a metric-aware score
(below).

**Deletion** — `remove(node_id)` adds a tombstone. The node stays in the graph as a
waypoint: searches and inserts traverse it, so paths through it keep working, but it is
never returned and never chosen as a new neighbour. Its vector and edges stay in memory. The engine rebuilds an index from records once its
tombstones outnumber its live entries.

### VectorStore and SearchParams

**Files:** [`vector_store.rs`](../crates/atlantis-vector/src/vector_store.rs),
[`params.rs`](../crates/atlantis-vector/src/params.rs)

`VectorStore` keeps `Vec<Vec<f32>>` with a fixed dimensionality. `insert` appends and
returns a sequential `NodeId`; `insert_at` writes a specific slot (used by
`HnswIndex::insert`); `remove` clears a slot; `contains` and `len` ignore empty slots.

`SearchParams { k, ef_search: Option<usize>, score_threshold: Option<f32> }` defaults to
`k = 10` and has a builder:

```rust
let params = SearchParams::top_k(10).with_threshold(0.7).with_ef_search(200);
```

### HNSW Parameters

`HnswParams` is defined in `atlantis-core` (`config.rs`) and re-exported by
`atlantis-vector`. `EngineConfig.vector.hnsw` holds the defaults; `CREATE VECTOR INDEX`
options override `m` (with `m0 = 2 * m`) and `efConstruction` per index.

| Parameter | Default | Effect |
|---|---|---|
| `m` | 16 | Connections per node on layers > 0. Higher = better recall, more memory. |
| `m0` | 32 | Connections per node on layer 0. Usually `2 * m`. |
| `ef_construction` | 200 | Candidate pool during insertion. Higher = better graph, slower build. |
| `ef_search` | 50 | Candidate pool during search (raised to at least `k`). Higher = better recall, slower. |
| `max_level` | 16 | Hard cap on layers. |

Tuning: raise `ef_search` (100–200) and `m` (24–48) for recall; lower `ef_search` for
latency; lower `ef_construction` for faster ingest; lower `m` (8–12) to save memory.

The crate's recall tests compare HNSW against brute force. `test_recall` in `hnsw.rs`
uses 500 random 32-dimensional vectors. `crates/atlantis-vector/tests/recall.rs` uses
4,000 clustered vectors with a low intrinsic dimension, like real embeddings, where
connectivity bugs show: it requires recall@10 ≥ 0.95, that ≥ 99.5% of nodes find
themselves, and recall@10 ≥ 0.90 after deleting 30%. End-to-end recall and latency at
larger scales come from the [Retrieval Bench](../benches/retrieval/README.md).

### Vector Index DDL

```cypher
CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};

CREATE VECTOR INDEX image_clip IF NOT EXISTS FOR (i:Image) ON (i.clip)
  OPTIONS {dimension: 512, metric: 'dot_product'};

DROP INDEX image_clip;
DROP INDEX image_clip IF EXISTS;
```

| Option | Default | Meaning |
|---|---|---|
| `dimension` | `EngineConfig.vector.dimensions` (128) | Vector length. Records whose vector has another length are not indexed. |
| `metric` | `EngineConfig.vector.distance_metric` (cosine) | `'cosine'`, `'euclidean'` or `'dot_product'` |
| `m` | `EngineConfig.vector.hnsw.m` (16) | Graph degree; layer 0 uses `2 * m` |
| `efConstruction` | `EngineConfig.vector.hnsw.ef_construction` (200) | Build-time candidate pool |

An unknown option, or a non-positive integer, is an error. Creating an index that
exists fails unless `IF NOT EXISTS` is given; dropping a missing one fails unless
`IF EXISTS` is given. The index covers records carrying the label, as their table or as
a secondary label, whose property holds a `Vector` or a list of numbers. The definition
(`VectorIndexDef`) is stored in the catalog, in the same transaction as other writes;
`CALL db.indexes()` lists the definitions. The option parser is `create_vector_index` in
[`crates/atlantis-engine/src/exec/mod.rs`](../crates/atlantis-engine/src/exec/mod.rs).

### Search Procedure

```cypher
CALL db.vector.search('chunk_embedding', $vec, 10) YIELD node, score
RETURN node.text AS text, score;

// optional fourth argument: a threshold, or a map {threshold, efSearch}
CALL db.vector.search('chunk_embedding', $vec, 10, {efSearch: 200, threshold: 0.75})
YIELD node, score
RETURN node, score;
```

| Argument | Meaning |
|---|---|
| 1 — index name or label | An index name. A label is accepted when exactly one index covers it. |
| 2 — query vector | A `Vector` or a list of numbers of the index's dimension (usually a parameter) |
| 3 — `k` | A positive integer (literal or parameter) |
| 4 — optional | A number (minimum score) or a map with `threshold` and `efSearch` |

- `node` is the record id of the hit, so `node.text` reads a property, further
  `MATCH` patterns can start from it, and `RETURN node` returns its document.
- `score` is higher-is-better: cosine similarity for `cosine`, the dot product for
  `dot_product`, and `1 / (1 + distance)` for `euclidean`. `threshold` applies to it.
- Without `YIELD`, both columns are bound; a query ending in `CALL` returns them.
- `db.vector.similarity(a, b)` (alias `vector.similarity.cosine`) is an expression
  function that returns the cosine similarity of two vectors.

### How the Index Is Maintained

**File:** [`crates/atlantis-engine/src/vector_index.rs`](../crates/atlantis-engine/src/vector_index.rs)

Only the definition is stored; the HNSW graph is **derived data** held in memory:

- **Built on first search.** The first search of an index (after open, create or
  redefinition) scans the records with its label and builds the graph.
- **Updated after each commit.** A commit reports the record ids it changed, and every
  built index of that database re-reads those records. An unchanged vector is left in
  place; a deleted or changed one is tombstoned and the new value inserted.
- **Committed data only.** A search sees committed records, not the uncommitted writes
  of the transaction that runs it.
- **Self-healing.** If updating an index after a commit fails, the index is dropped and
  rebuilt on the next search; the commit itself is never reported as failed. An index
  whose tombstones outnumber its live entries is also rebuilt.
- **Restart.** Nothing but the definition survives a restart; the first search rebuilds
  the graph from records.

Builds and updates run synchronously on the calling task. Each built index has its own
read-write lock: searches of an index run in parallel, a build holds its index's write
lock (other indexes stay available), and a commit write-locks only the indexes of its
database while it applies its changes ([16 — Engine Runtime](16-engine-runtime.md)).

## Target Design

### Planned Extensions to DDL and Search (Phase 3)

- Options `m0`, `efSearch` (a per-index default for query time) and `quantization`
  (see [Quantization](#quantization-planned--after-phase-3-unscheduled)).
- Taking `dimension` from a typed field (`vector<f32, N>`, Phase 2) and failing a write
  whose vector has the wrong dimension, instead of skipping the record.
- Index maintenance inside the writing transaction once the graph lives in KV (below).

### Filtering (Planned — Phase 3)

Filters are written as ordinary Cypher. No extra syntax is needed:

```cypher
CALL db.vector.search('chunk_embedding', $vec, 10) YIELD node AS c, score
WHERE c.lang = 'en' AND c:Published AND c.createdAt > $since
RETURN c, score;
```

The index is already scoped to one table, so the table label is an implicit filter.
For the remaining predicates the optimizer chooses one of two strategies
(see [15 — Query Planner & Executor](15-query-planner-executor.md)):

| Strategy | When | How |
|---|---|---|
| **Pre-filter** | The predicate is selective and answerable from a secondary index or the secondary-label set | Build an allow-set of record keys first, then run HNSW traversal that only admits allowed nodes into the result heap while still walking through others for connectivity. If the allow-set is very small, skip HNSW and compute exact distances over it. |
| **Post-filter** | The predicate is broad or not indexable | Search with an enlarged candidate pool (`k × overfetch`, growing until `k` rows survive or the index is exhausted), then evaluate `WHERE`. |

Both strategies return at most `k` rows that satisfy the predicate. Today a `WHERE`
after `YIELD` filters the `k` hits after the search, so fewer than `k` rows can remain.

### Persistence Through `atlantis-kv` (Planned — Phase 3)

> **Revised 2026-11-16.** On local backends (`mem`, `rocksdb`) the focus-track design
> replaces per-insert `vx/` writes with a checkpointed index image plus replay, and
> adds versioned entries and a transaction overlay for snapshot-consistent,
> read-your-writes search. Filtering is chosen per query between exact, filtered
> traversal and post-filter. See
> [02 — Architecture § Hybrid Retrieval Path](02-architecture.md#hybrid-retrieval-path-focus).
> The `vx/` layout below stays the design for the distributed backend (Phase 7).

Today each process rebuilds the graph from records. In the target design each named
index stores its graph under the `vx/` prefix of its table in `atlantis-kv`
([04 — Storage Engine](04-storage-engine.md)):

```
/ns/{ns}/db/{db}/tb/{tb}/vx/{index}/meta              -> params, entry point, max level
/ns/{ns}/db/{db}/tb/{tb}/vx/{index}/v/{key}           -> vector (or quantized code)
/ns/{ns}/db/{db}/tb/{tb}/vx/{index}/n/{level}/{key}   -> neighbor list at a layer
```

Index entries are keyed by the record key, so the HNSW `NodeId` indirection disappears
from the public surface. Index mutations commit in the same KV transaction as the
record, so the index can never point at a record that does not exist. A deletion writes
a tombstone; a background compaction worker
(see [16 — Engine Runtime](16-engine-runtime.md)) reconnects neighbors and removes the
tombstoned entries.

Each process keeps an in-memory copy of the hot layers for speed and checks the stored
version on read, which is also how cluster nodes share an index
(see [20 — Distributed Architecture](20-distributed-architecture.md)).

### Quantization (Planned — after Phase 3, unscheduled)

Quantization trades a little recall for much less memory.

| `quantization` | Stored per vector | Notes |
|---|---|---|
| `'none'` | `4 × N` bytes | Exact distances |
| `'scalar'` | `N` bytes (int8 per component) | Per-index min/max calibration |
| `'pq'` | `pqSubvectors` bytes | Product quantization; codebooks trained on a sample during the build |

With quantization, traversal uses the compressed codes and the top candidates are
re-ranked with the full-precision vectors, which stay in `v/`.

### Hybrid Search (Planned — Phase 3)

Combining vector similarity with BM25 keyword relevance through Reciprocal Rank Fusion
(`db.search.hybrid`) is documented in
[11 — Full-Text Search](11-full-text-search.md#hybrid-search-with-reciprocal-rank-fusion).

### Concurrency

Today each built index sits behind its own read-write lock in the engine: searches
share it, and builds and post-commit updates take it exclusively. In the target runtime, writers go through the
commit sequencer and readers search a consistent snapshot concurrently
(see [05 — Transactions & Consistency](05-transactions-consistency.md) and
[16 — Engine Runtime](16-engine-runtime.md)).

## Design Decisions

**Why HNSW?** It handles incremental inserts without rebalancing and gives high recall
at low latency, which suits continuously updated RAG and agent-memory data.

**Why tombstones?** Removing a node from a multi-layer graph requires reconnecting its
neighbors on every layer. Tombstones make deletes cheap; today a heavily tombstoned
index is rebuilt from records, and the target design repairs it with a background
compaction.

**Why derive the index from records instead of storing it?** Records are the source of
truth and are already durable, so an index can never disagree with the data after a
crash; the cost is a rebuild on first search after open.

**Why cosine by default?** Most text embedding models are trained for cosine similarity,
and cosine is insensitive to vector magnitude.

**Why one index per label and field instead of one engine-wide index?** Different
embedding models have different dimensions and metrics, and scoping an index to one
label makes the label filter free.

**Why filter with plain `WHERE` instead of a filter argument?** It keeps one query
language and lets the optimizer choose pre- or post-filtering from statistics rather
than forcing the user to choose.

## What's Next

- [11 — Full-Text Search](11-full-text-search.md) — BM25 and hybrid text + vector search.
- [06 — Query Language](06-query-language.md) — `CALL ... YIELD` and procedure syntax.
- [15 — Query Planner & Executor](15-query-planner-executor.md) — how vector search runs today and the planned operators.
- [22 — Configuration](22-configuration.md) — `VectorConfig` and HNSW options.
- [24 — RAG Patterns](24-rag-patterns.md) — vector search combined with graph traversal.
