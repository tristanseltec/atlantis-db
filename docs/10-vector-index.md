# 10 — Vector Index

> **Status:** Partially implemented — HNSW algorithm implemented; index wiring and persistence Phase 0; DDL, named indexes, KV persistence and filtering Phase 3
> **Crates:** `atlantis-vector`, `atlantis-engine`, `atlantis-cypher`, `atlantis-kv` (planned)

This chapter documents the HNSW (Hierarchical Navigable Small World) vector index:
the algorithm as it exists today, how queries reach it, and the target design for
named vector indexes, persistence, filtering, quantization and hybrid search.

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

### HNSW Index

**File:** [`crates/atlantis-vector/src/hnsw.rs`](../crates/atlantis-vector/src/hnsw.rs)

`HnswIndex` holds one adjacency map per layer (`HashMap<NodeId, Vec<NodeId>>`), a
`VectorStore`, the entry point (the node with the highest level), the current maximum
level, the distance metric, and a `deleted` tombstone set.

**Insertion** — `insert(node_id, vector)`. The caller supplies the ID.

1. Validate dimensions against the index (`AtlantisError::DimensionMismatch`).
2. Store the vector at `node_id` in the `VectorStore`.
3. Draw a level `floor(-ln(uniform(0,1)) * mL)` with `mL = 1/ln(m)`, capped at `max_level`.
4. The first insert becomes the entry point.
5. Otherwise, greedy-descend from the top layer to `level + 1`, then at each layer from
   `level` down to 0 search with an `ef_construction` candidate pool, connect to `m`
   neighbors (`m0` at layer 0) bidirectionally, and prune neighbors that exceed the limit.
6. If the new level exceeds the current maximum, the node becomes the entry point.

**Search** — `search(query, k, ef_search: Option<usize>)` returns `(NodeId, distance)`
pairs sorted ascending. It greedy-descends to layer 1, searches layer 0 with
`ef = ef_search.unwrap_or(params.ef_search).max(k)`, drops tombstoned and empty slots,
then truncates to `k`.

**Threshold search** — `search_with_params(query, &SearchParams)` runs `search` and, when
`score_threshold` is set, keeps results where `1.0 - distance >= threshold`. That
conversion is a cosine similarity; for the other metrics the threshold has no
calibrated meaning today.

**Deletion** — `remove(node_id)` adds a tombstone. Search filters tombstones; the vector
and its edges stay in memory. Compaction is not implemented.

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
`atlantis-vector`.

| Parameter | Default | Effect |
|---|---|---|
| `m` | 16 | Connections per node on layers > 0. Higher = better recall, more memory. |
| `m0` | 32 | Connections per node on layer 0. Usually `2 * m`. |
| `ef_construction` | 200 | Candidate pool during insertion. Higher = better graph, slower build. |
| `ef_search` | 50 | Candidate pool during search (raised to at least `k`). Higher = better recall, slower. |
| `max_level` | 16 | Hard cap on layers. |

Tuning: raise `ef_search` (100–200) and `m` (24–48) for recall; lower `ef_search` for
latency; lower `ef_construction` for faster ingest; lower `m` (8–12) to save memory.

The crate's recall test (`test_recall` in `hnsw.rs`) compares HNSW against brute force
on 500 random 32-dimensional vectors with `ef_search = 100` and asserts average recall
above 80%. No larger benchmark exists in the repository.

### How Queries Reach the Index

The engine actor (`crates/atlantis-engine/src/actor.rs`) creates **one** engine-wide
`HnswIndex` from `EngineConfig.vector` (`dimensions` default 128, `distance_metric`
default cosine, `hnsw` parameters). The executor receives a read-only reference.

The planner (`crates/atlantis-cypher/src/planner/mod.rs`) accepts:

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7) YIELD node, score
RETURN node, score
```

| Argument | Today |
|---|---|
| 1 — label string | Required literal, but **ignored** by the executor (no label filtering) |
| 2 — query vector | Expression that must evaluate to a `Value::Vector` (usually a parameter) |
| 3 — `k` | Required integer literal |
| 4 — threshold | Optional float or integer literal |

The first `YIELD` name receives the HNSW `NodeId` as an **integer**, not a node, and the
second receives `1 - distance` as a float. `db.vector.similarity(a, b)` is available as
an expression function and returns the cosine similarity of two vector values.

### Known Gaps (Phase 0)

- **Nothing populates the index.** The engine never calls `HnswIndex::insert`, so a
  vector property stored on a node is not searchable through `db.vector.search` yet.
- **Persistence is a stub.** In `persistence.rs`, `serialize_index` returns an empty
  vector and `deserialize_index` returns `AtlantisError::Serialization`. The index is
  in-memory only and starts empty on every open.
- Label filtering, yielding nodes instead of IDs, and metric-aware scores are missing.

Phase 0 closes these gaps: indexing vector properties on write, yielding the node, and
serializing the layers, vectors and tombstones with the current cold tier
(see [04 — Storage Engine](04-storage-engine.md)).

## Target Design

### Vector Index DDL (Planned — Phase 3)

Vector indexes become named, schema-level objects. A database can hold many of them,
each bound to one table (the node's first label) and one `vector<f32, N>` field.

```cypher
CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};

CREATE VECTOR INDEX image_clip FOR (i:Image) ON (i.clip)
  OPTIONS {dimension: 512, metric: 'dot_product'};

DROP INDEX image_clip;
```

| Option | Default | Meaning |
|---|---|---|
| `dimension` | from field type `vector<f32, N>` | Required when the field type does not fix `N` |
| `metric` | `'cosine'` | `'cosine'`, `'euclidean'`, `'dot_product'` (the `name()` values above) |
| `m`, `m0` | 16, `2 * m` | Graph degree per layer |
| `efConstruction` | 200 | Build-time candidate pool |
| `efSearch` | 50 | Default query-time candidate pool |
| `quantization` | `'none'` | See [Quantization](#quantization-planned--after-phase-3-unscheduled) |

Creating an index on a populated table schedules a background build in `atlantis-index`'s
index manager (see [07 — Schema & DDL](07-schema-ddl.md)). Writes after creation are
indexed in the same transaction as the record. A record whose field is missing or
`null` is simply not indexed; a vector of the wrong dimension fails the write.

`EngineConfig.vector` remains as the default for the options above.

### Search Procedure (Planned — Phase 3)

```cypher
CALL db.vector.search('chunk_embedding', $vec, 10) YIELD node, score
RETURN node.text, score;

// optional fourth argument: a number (threshold, as today) or an options map
CALL db.vector.search('chunk_embedding', $vec, 10, {efSearch: 200, threshold: 0.75})
YIELD node, score
RETURN node, score;
```

- The first argument names an index, not a label.
- `node` is the indexed record, so properties and further pattern matching work.
- `score` is a similarity where higher is better: `1 - distance` for cosine,
  `1 / (1 + distance)` for Euclidean, and the raw dot product for `dot_product`.
  `threshold` applies to this score.

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

Both strategies return at most `k` rows that satisfy the predicate, unlike today's
behaviour where filtering would happen after truncation.

### Persistence Through `atlantis-kv` (Planned — Phase 3)

Once storage moves to `atlantis-kv` ([04 — Storage Engine](04-storage-engine.md)),
each named index stores its graph under the `vx/` prefix of its table:

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

Today the HNSW index is owned by the single-writer engine actor, so it needs no internal
locking. In the target runtime, writers go through the commit sequencer and readers
search a consistent snapshot concurrently
(see [05 — Transactions & Consistency](05-transactions-consistency.md) and
[16 — Engine Runtime](16-engine-runtime.md)).

## Design Decisions

**Why HNSW?** It handles incremental inserts without rebalancing and gives high recall
at low latency, which suits continuously updated RAG and agent-memory data.

**Why tombstones?** Removing a node from a multi-layer graph requires reconnecting its
neighbors on every layer. Tombstones make deletes cheap, and compaction does the
repair in the background.

**Why cosine by default?** Most text embedding models are trained for cosine similarity,
and cosine is insensitive to vector magnitude.

**Why one index per table field instead of one engine-wide index?** Different
embedding models have different dimensions and metrics, and scoping an index to one
table makes the label filter free.

**Why filter with plain `WHERE` instead of a filter argument?** It keeps one query
language and lets the optimizer choose pre- or post-filtering from statistics rather
than forcing the user to choose.

## What's Next

- [11 — Full-Text Search](11-full-text-search.md) — BM25 and hybrid text + vector search.
- [06 — Query Language](06-query-language.md) — `CALL ... YIELD` and procedure syntax.
- [15 — Query Planner & Executor](15-query-planner-executor.md) — how vector search is planned.
- [22 — Configuration](22-configuration.md) — `VectorConfig` and HNSW options.
- [24 — RAG Patterns](24-rag-patterns.md) — vector search combined with graph traversal.
