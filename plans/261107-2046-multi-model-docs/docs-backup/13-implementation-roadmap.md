# 13 — Implementation Roadmap

This chapter documents the current implementation status of each crate, identifies stubs and TODOs in the codebase, and outlines the remaining work to reach full v1 functionality.

## Implementation Status Summary

| Crate | Status | Unit Tests |
|---|---|---|
| `atlantis-core` | Complete | Comprehensive (all types) |
| `atlantis-storage` | Substantially complete | Comprehensive (all modules) |
| `atlantis-vector` | Substantially complete; persistence stub | Comprehensive (including recall test) |
| `atlantis-cypher` | Substantially complete; optimizer pass-through | Parser and tokenizer tests |
| `atlantis-engine` | Core actor + executor working; background actors stub | Minimal |
| `atlantis-db` | Public API complete | Minimal |
| `benches` | Scaffolding exists | N/A (benchmarks) |

## Per-Crate Status

### atlantis-core — Complete

All core types are fully implemented with comprehensive unit tests:

- `Value` with all variants, comparison, conversions, and serde
- `Node`, `NodeId`, `NodeRef`
- `Relationship`, `EdgeId`
- `Label`, `LabelSet` (BTreeSet-backed)
- `RelType`
- `PropertyMap`
- `Direction`
- `EngineConfig`, `StorageConfig`, `VectorConfig`, `HnswParams`, `EngineConfigBuilder`
- `AtlantisError` with thiserror

**No TODOs or stubs.**

### atlantis-storage — Substantially Complete

Implemented:
- `HotTier`: node/edge storage, CSR + delta adjacency, label index, LRU tracker, compaction, memory estimation
- `CsrAdjacency` / `CsrStore`: CSR build, query, iterate, serde
- `DeltaLog`: insert, get, drain, remove, serde
- `ColdTier`: append-only file, page index, read/write/contains/remove
- `LruTracker`: touch, evict candidates, remove
- `TieringManager`: eviction (maybe_evict), promotion (partial)
- `GraphStore`: unified facade with all operations
- `persistence`: checkpoint/restore with magic bytes and version

TODOs and limitations:
- `GraphStore::get_node`: Cold tier promotion does not preserve the original node ID (sequential ID allocation conflict). The method has a `// TODO: proper promotion that preserves node ID` comment.
- `TieringManager::promote`: Returns the node data but does not re-insert into the hot tier with the original ID.
- `ColdTier`: No garbage collection of unreferenced pages (deleted data remains in the file).

### atlantis-vector — Substantially Complete; Persistence Stub

Implemented:
- `HnswIndex`: full insertion, search (top-k and threshold), deletion (tombstone), random level assignment, greedy search, layer search, neighbor selection, connection pruning
- `DistanceMetric` trait with `CosineDistance`, `EuclideanDistance`, `DotProductDistance`
- `VectorStore`: compact storage, insert, get, remove, contains
- `SearchParams`: top-k, threshold, ef_search
- Recall test comparing HNSW against brute-force (>80% threshold)

Stubs:
- `persistence::serialize_index`: Returns empty `Vec<u8>`. Comment: `// TODO: Implement full HNSW serialization`
- `persistence::deserialize_index`: Returns error `"HNSW deserialization not yet implemented"`

The HNSW index is not persisted. On engine restart, the index is empty and vectors must be re-inserted. This is a significant gap for production use.

### atlantis-cypher — Substantially Complete; Optimizer Pass-Through

Implemented:
- Lexer: full tokenizer with keywords, punctuation, strings, numbers, parameters, comments, arrows
- Parser: recursive-descent with operator precedence (OR, AND, comparison, additive, multiplicative, unary, primary)
- AST: all clause types, pattern types, expression types
- Planner: MATCH, WHERE, CREATE (nodes), SET, DELETE, CALL (vector search), WITH, UNWIND, RETURN with ORDER BY/SKIP/LIMIT
- Parser tests: match, create, where, order by, parameters, call, set, delete, detach delete, with

Pass-through:
- `optimizer::optimize`: Returns the plan unchanged. Comment: `// For v1, the planner already produces reasonable plans.`

Parsed but not planned/executed:
- `MERGE`: Parsed into `MergeClause`, not handled by the planner
- `REMOVE`: Parsed into `RemoveClause`, not handled by the planner
- Variable-length path syntax (`*min..max`): Parsed into `PathLength`, planner emits `VarLenExpand` but executor returns empty
- `UNION` / `ALL`: Tokenized and parsed but not handled in statement parsing
- `OPTIONAL MATCH`: Parsed (`optional: true` flag) but no special handling in executor

### atlantis-engine — Core Working; Background Actors Stub

Implemented:
- `CoreEngineActor`: owns store + vector index, processes commands sequentially
- `Command` enum: Query, CreateNode, GetNode, Checkpoint, Shutdown
- `QueryExecutor`: walks logical plan, executes all implemented operators, evaluates expressions
- `EngineHandle`: clonable, all async methods
- `spawn_engine`: creates actor, spawns on tokio, returns handle

Stubs:
- `tiering_actor::run_tiering_loop`: Empty loop with 60s interval. Comment: `// Tiering is handled inline by the store in v1.`
- `checkpoint_actor::run_checkpoint_loop`: Empty loop with 300s interval. Comment: `// Checkpointing is handled explicitly by the user or on shutdown in v1.`

Executor TODOs (identified by comments):
- `VarLenExpand`: `// TODO: implement variable-length expansion` — returns empty results
- `CreateRelationship`: `// TODO: implement relationship creation` — returns empty results
- `Delete` with `detach: true`: `// For v1, delete_node handles this` — edges are not explicitly removed before node deletion
- Vector search label filtering: `// Filter by label — for v1 we return all results` — the label argument to `db.vector.search` is parsed but not used to filter results

### atlantis-db — Public API Complete

Implemented:
- `Engine`: open, open_with_config, open_in_memory, execute, checkpoint, shutdown
- `Config` (alias for `EngineConfig`) with builder
- `ResultSet`: columns, rows, row_count, is_empty
- `Row`: get, get_string, get_int, get_float, get_bool, get_vector, values, column_count
- `Params`: from slice, from map

**No TODOs or stubs.**

## Remaining Tasks

### High Priority (blocking core functionality)

| Task | Crate | Description |
|---|---|---|
| HNSW persistence | atlantis-vector | Serialize/deserialize the HNSW graph structure and vectors to disk. Without this, the vector index is lost on restart. |
| Relationship creation in CREATE | atlantis-engine | Wire `CreateRelationship` in the executor so `CREATE (a)-[:R]->(b)` works end-to-end. |
| Cold tier promotion with stable IDs | atlantis-storage | Fix `TieringManager::promote` and `GraphStore::get_node` to re-insert evicted nodes with their original IDs. |

### Medium Priority (important for production use)

| Task | Crate | Description |
|---|---|---|
| Background checkpoint actor | atlantis-engine | Wire `run_checkpoint_loop` to send periodic `Checkpoint` commands to the core actor based on `checkpoint_interval_secs`. |
| Background tiering actor | atlantis-engine | Wire `run_tiering_loop` to send periodic eviction commands. Move inline `maybe_evict` calls to the background. |
| DETACH DELETE edge removal | atlantis-engine | Before deleting a node with `detach: true`, find and remove all connected edges. |
| Vector search label filtering | atlantis-engine | Use the label argument in `db.vector.search` to filter results by node label. Requires tracking labels on embedding nodes in the index. |
| MERGE execution | atlantis-cypher, atlantis-engine | Plan and execute MERGE as upsert: match-or-create. |
| Variable-length expand | atlantis-engine | Implement multi-hop traversal for `*min..max` path patterns. |
| Cold tier garbage collection | atlantis-storage | Periodically compact the append-only file to remove unreferenced pages. |

### Lower Priority (quality and completeness)

| Task | Crate | Description |
|---|---|---|
| Query optimizer rules | atlantis-cypher | Implement filter pushdown, limit pushdown, vector index detection in WHERE, dead code elimination. |
| Aggregation functions | atlantis-cypher, atlantis-engine | Implement `count`, `sum`, `avg`, `min`, `max`, `collect` with grouping. |
| OPTIONAL MATCH null fills | atlantis-engine | Return null values for unmatched optional patterns. |
| REMOVE clause execution | atlantis-cypher, atlantis-engine | Plan and execute property/label removal. |
| UNION support | atlantis-cypher | Combine results of multiple queries. |
| Built-in functions | atlantis-cypher, atlantis-engine | `id()`, string functions (`toUpper`, `toLower`, `trim`), list functions (`size`, `range`). |
| LFU eviction policy | atlantis-storage | Implement `EvictionPolicy::Lfu` as an alternative to LRU. |
| Tracing integration | all crates | Wire the `tracing` crate throughout for structured logging. Already a dependency in several crates. |

## Testing Status

### Unit Tests

| Crate | Coverage |
|---|---|
| atlantis-core | Comprehensive — every type has tests for creation, manipulation, serde roundtrips, edge cases |
| atlantis-storage | Comprehensive — CSR, delta, hot tier, cold tier, LRU, tiering, persistence all tested |
| atlantis-vector | Comprehensive — distance metrics, vector store, HNSW insert/search/delete, recall test |
| atlantis-cypher | Good — tokenizer and parser tests for all major clause types |
| atlantis-engine | Minimal — no dedicated unit tests; tested through the public API |
| atlantis-db | Minimal — no dedicated unit tests; tested through examples/docs |

### Integration Tests

**No `tests/` directory exists.** Integration tests that exercise the full pipeline (open engine, create data, query, verify results) are needed. The `benches` crate has benchmark scaffolding but no integration test suite.

### Property-Based Tests

`proptest` is a workspace dependency and dev-dependency of `atlantis-vector`, but no property-based tests are currently written. The requirements specify using proptest for invariant checking on the parser and storage engine.

### Benchmarks

Three benchmark files exist in `benches/benches/`:
- `storage_bench.rs`
- `vector_bench.rs`
- `query_bench.rs`

These are configured with criterion (`harness = false`, `async_tokio` feature). The benchmark content should be verified against the current API.

## Roadmap Checklist

From the project README, aligned with current status:

- [x] Requirements & architecture design
- [x] Core types and property graph model — `atlantis-core` complete
- [x] Hybrid tiered storage engine — `atlantis-storage` substantially complete
- [x] HNSW vector index — `atlantis-vector` substantially complete (persistence pending)
- [x] Cypher lexer and parser — `atlantis-cypher` lexer and parser complete
- [x] Query planner and executor — planner complete, executor substantially complete
- [x] Actor-based engine runtime — core actor complete, background actors stub
- [ ] Public API and documentation — API complete, this documentation is the docs effort
- [ ] Benchmarks and performance tuning — scaffolding exists, needs verification and tuning
- [ ] Standalone server mode (v2) — not started
- [ ] Client SDKs (v2) — not started
- [ ] Distributed clustering (v3) — not started

## What's Next

- **[02 — Architecture Overview](02-architecture.md)** — The system design these tasks implement.
- **[01 — Getting Started](01-getting-started.md)** — Start using what's built.
