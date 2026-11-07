# 25 — Implementation Roadmap

> **Status:** Planned — Phases 0–7 (this chapter records current status and the phased plan)
> **Crates:** all workspace crates; `atlantis-kv`, `atlantis-index`, `atlantis-text`, `atlantis-geo`, `atlantis-auth`, `atlantis-server` (planned)

This chapter has two parts. **Current status** records what the code in `crates/`
does today, verified against the source. **Phased roadmap** is the plan that takes
Atlantis DB from a pre-release embedded graph + vector engine to the multi-model
database the other chapters describe. Every "Planned — Phase N" marker elsewhere in the
docs refers to a phase defined here.

## Current status

### Build and tests

The workspace does not compile as committed. `atlantis-core/src/config.rs` uses
`AtlantisError` without importing it, and every other crate depends on `atlantis-core`.
With that one import added, the library crates up to `atlantis-engine` build, and these
problems remain:

| Crate | Unit tests (with the import added) |
|---|---|
| `atlantis-core` | Pass. |
| `atlantis-cypher` | Two failures: `test_parse_set` (the `SET` parser consumes `=` as a comparison) and `test_tokenize_arrows`. |
| `atlantis-storage` | The test build fails: tests reference `atlantis_core::EvictionPolicy`, which is not re-exported, and two type errors. |
| `atlantis-vector` | The test build fails: tests use `bincode`, which is not a dev-dependency. |
| `atlantis-engine` | Builds. No tests. |
| `atlantis-db` | Library does not compile (four errors, listed in [21 — API Reference](21-api-reference.md#known-build-defects-phase-0)). |

There is no integration test directory. `benches/benches/` holds `storage_bench.rs`,
`vector_bench.rs` and `query_bench.rs` as Criterion scaffolding.

### Per crate

**`atlantis-core`.** `Value` (`Null`, `Bool`, `Integer`, `Float`, `String`, `Array`,
`Vector`), `Node`/`NodeId`, `Relationship`/`EdgeId`, `Label`/`LabelSet`, `RelType`,
`PropertyMap`, `Direction`, `EngineConfig` with builder and validation, and
`AtlantisError`. The only defect is the missing import. `EngineConfigBuilder` and
`EvictionPolicy` are reachable only through `atlantis_core::config`.

**`atlantis-storage`.** The hot tier (node and edge vectors, CSR+delta adjacency per
relationship type, label index, LRU tracker), the append-only cold tier, the tiering
manager and checkpoint/restore all exist. Gaps:

- Cold-tier promotion is a `TODO` in `GraphStore::get_node` and does not preserve node
  ids.
- Deleting a node only tombstones it.
- Restoring a checkpoint gives nodes new ids while edges keep the old ones.
- Nothing calls `maybe_evict`, so eviction never runs.
- The cold tier has no garbage collection.

**`atlantis-vector`.** HNSW insert, search, threshold search and tombstone delete;
cosine, Euclidean and dot-product distances; a brute-force recall test. Persistence is
a stub: `serialize_index` returns empty bytes and `deserialize_index` returns an error.

**`atlantis-cypher`.** The lexer, a recursive-descent parser and a planner. The
optimizer passes the plan through unchanged. Parser gaps:

- `MERGE`, `REMOVE` and `SET n.p = v` fail to parse. The expression parser consumes the
  `=` of `SET`.
- `<>`, `IN`, `IS NULL`, `XOR`, `STARTS WITH`, `ENDS WITH`, `CONTAINS` and `count(*)`
  fail to parse.
- `<-`, `--` and `-[]-` patterns, and comma-separated patterns, fail to parse.
- `*1..3` is a lexer error, because it is read as a float.
- Dotted function names outside `CALL` fail to parse.
- Everything the `RETURN` parser does not recognise is silently dropped, including
  `UNION`, `CASE`, `ASC`, `n.a.b` and `SKIP` after `LIMIT`.

Planner gaps:

- Inline property maps in `MATCH` are ignored.
- Relationship elements in `CREATE` are dropped.
- `SET` is planned on an empty input.
- Variable-length lengths are ignored.
- `OPTIONAL MATCH` is treated as `MATCH`.
- With several `MATCH` clauses, only the last scan's rows are kept.
- `DISTINCT` is ignored, and so are `ORDER BY` and `LIMIT` on `WITH`.

**`atlantis-engine`.** The single-writer Tokio actor, command channel, `EngineHandle` and
`QueryExecutor` exist. The checkpoint and tiering actors are empty loops that are never
spawned. `spawn_engine` is not re-exported at the crate root. Executor gaps:

- `VarLenExpand` and `CreateRelationship` are `TODO`s.
- `Expand` binds the target to a placeholder, so multi-hop `MATCH` returns every node
  of the target label. It also follows outgoing edges only and returns nothing for an
  untyped relationship.
- `UNWIND` or `RETURN` as the first clause returns no rows.
- No function except `db.vector.similarity` is evaluated, and that one cannot be parsed.
  `%` and map literals evaluate to `null`.
- `ORDER BY` resolves only returned aliases.
- No API creates an edge except the internal `GraphStore::insert_edge`.
- A descending `Sort` key returns early even on ties, so secondary keys are ignored.
- `CREATE` returns only the last node created.
- `DETACH DELETE` leaves edges.
- The engine never inserts vectors into the HNSW index, so `db.vector.search` returns no
  rows, and its label argument is ignored.

**`atlantis-db`.** The facade source declares `Engine`, `Config`, `ResultSet`, `Row` and
`Params`, but does not compile. Only `disk_path` and the vector settings from
`EngineConfig` reach the engine.

### Chapter status

| Chapter | Status | Completed by |
|---|---|---|
| [01](01-getting-started.md), [02](02-architecture.md), [03](03-data-model.md), [04](04-storage-engine.md), [06](06-query-language.md), [15](15-query-planner-executor.md), [16](16-engine-runtime.md) | Partial | Phases 0–2, then later phases for platform parts |
| [05](05-transactions-consistency.md) | Phase 1 | Phase 1 |
| [07](07-schema-ddl.md), [08](08-document-relational.md) | Phase 2 | Phase 2 |
| [09](09-graph-model.md), [10](10-vector-index.md) | Partial | Phase 0 (core) and Phase 3 (paths, algorithms, vector DDL) |
| [11](11-full-text-search.md), [12](12-geospatial.md), [13](13-key-value-time-series.md) | Phase 3 | Phase 3 |
| [14](14-live-queries-events.md) | Phase 4 | Phase 4 |
| [17](17-server-protocols.md), [18](18-security-auth.md) | Phase 5 | Phase 5 |
| [19](19-client-sdks.md) | Phase 6 | Phase 6 |
| [20](20-distributed-architecture.md) | Phase 7 | Phase 7 |
| [21](21-api-reference.md), [22](22-configuration.md), [23](23-query-cookbook.md), [24](24-rag-patterns.md) | Partial | Grow with every phase |

## Phased roadmap

### Dependency graph

```
Phase 0  Complete graph + vector core
   │
Phase 1  Transactional storage foundation (atlantis-kv, MVCC)
   │
Phase 2  Unified record model, document/relational, schema, atlantis-index
   │
   ├──────────────┬──────────────────────────┐
Phase 3          Phase 4                     Phase 5
Search &         Reactive                    Server & security
specialized      (change feeds, events,      (atlantis-server,
models           live queries)               atlantis-auth)
   │                 │                            │
   └─────────────────┴──────────┬─────────────────┘
                                │
                     Phase 6  SDKs (needs 5; uses 3 and 4)
                                │
                     Phase 7  Distributed (needs 1 and 5; uses 4)
```

Phases 3, 4 and 5 depend only on Phase 2 and can run in parallel. Phase 6 needs the
server protocol from Phase 5, and its SDK surface grows as Phases 3 and 4 land. Phase 7
needs the `atlantis-kv` abstraction (Phase 1) and the stateless server (Phase 5). It
also needs Phase 4 so that live queries can be distributed.

### Phase 0 — Complete graph + vector core

**Goal.** Make today's embedded engine correct and usable before changing its
foundations.

**Deliverables.**

1. A green build:
   - Import `AtlantisError` in `atlantis-core/src/config.rs`.
   - Remove the inherent `impl Config { fn builder() }` on the foreign type alias
     (E0116), which also clashes with `EngineConfig::builder`.
   - Stop re-exporting `EngineConfigBuilder` from `atlantis-core`'s root, or export it
     there first.
   - Re-export `spawn_engine`.
   - Fix the `ResultSet::rows` item type.
   - Fix the storage and vector test builds and the two failing Cypher tests.
2. Correct graph semantics:
   - Fix the multi-hop `MATCH` binding, and keep rows across several `MATCH` clauses.
   - Parse `<-`, `--`, `-[]-` and comma-separated patterns. Make `Expand` honour
     direction (incoming, both) and untyped relationships.
   - Apply inline property maps in `MATCH`.
   - Variable-length expand, including lexing and parsing `*min..max`.
   - Path variables (`p = (...)`) and the functions `length`, `nodes` and
     `relationships`.
3. Writes:
   - `CREATE` relationships, comma-separated patterns, and `MATCH ... CREATE`.
   - Return every created element, not only the last node.
   - Plan `MATCH ... SET` on its real input and fix the `SET` parser.
   - Parse and execute `MERGE` (with `ON CREATE` / `ON MATCH`) and `REMOVE`.
   - `DETACH DELETE` removes edges.
   - Delete removes the node physically instead of only tombstoning it.
4. Query features:
   - Leading `RETURN` and `UNWIND`.
   - A complete `RETURN`/`WITH` tail: `UNION`, `CASE`, `ASC`, nested property access
     (`n.a.b`), `SKIP`/`LIMIT` in either order, and `ORDER BY`/`LIMIT`/`DISTINCT` on
     `WITH`. Unknown trailing tokens become parse errors instead of being dropped.
   - `ORDER BY` on any expression, not only returned aliases.
   - `OPTIONAL MATCH` null rows and `DISTINCT`.
   - Aggregation (`count`, `sum`, `avg`, `min`, `max`, `collect`) with implicit
     grouping.
   - The operators `<>`, `IN`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `IS NULL`,
     `XOR` and `%`, and `count(*)`.
   - Core scalar functions (`id`, `size`, `toUpper`, `toLower`, `trim`, `coalesce`,
     `abs`).
   - Dotted function names (`db.vector.similarity`).
   - Fix `Sort` so ties on a descending key fall through to the next key.
5. Vector:
   - Index `Vector` properties into HNSW on create, update and delete.
   - Honour the label argument of `db.vector.search` and accept a parameter for `k`.
   - Yield the node rather than an internal integer id.
   - Persist HNSW (`serialize_index`/`deserialize_index`) in the checkpoint.
6. Storage and runtime:
   - Preserve node ids through checkpoint restore and cold-tier promotion.
   - Spawn the checkpoint and tiering actors, driven by `checkpoint_interval_secs` and
     `memory_budget_bytes`.
   - Pass the full `StorageConfig` to `GraphStore`.

**Crates.** `atlantis-core`, `atlantis-storage`, `atlantis-vector`, `atlantis-cypher`,
`atlantis-engine`, `atlantis-db`.

**Docs.** 01, 04, 06, 09, 10, 15, 16, 21, 22, 23, 24.

**Exit criteria.** `cargo test --workspace` passes. Every "Phase 0" row in
[23 — Query Cookbook](23-query-cookbook.md#recipes-that-do-not-work-yet) passes as an
integration test through `Engine::execute`. A restart restores nodes, edges and the
HNSW index with identical ids.

**Testing and benchmarks.**

- Add `crates/atlantis-db/tests/` with one integration test per cookbook recipe.
- Add proptest round-trips for checkpoint/restore (ids and adjacency preserved) and for
  parser → planner on generated patterns.
- Fill the three Criterion benches and record a baseline: node insert, 1–3 hop expand,
  HNSW insert and top-10 search with recall@10.

### Phase 1 — Transactional storage foundation

**Goal.** Replace the hot/cold tier with an ordered, transactional KV layer, so every
model shares ACID storage.

**Deliverables.**

- The new crate `atlantis-kv`, with `KvStore`/`Transaction` traits (`begin`, `get`,
  `set`, `delete`, `scan`, `commit`, `cancel`) and the memcomparable key encoding from
  [04 — Storage Engine](04-storage-engine.md).
- MVCC with snapshot isolation by default and optional serializable isolation.
- Backends behind features `kv-mem` and `kv-rocksdb`.
- Migrate the graph store onto KV. Keep CSR+delta as an adjacency cache rebuilt from
  `g/` keys, and persist HNSW under `vx/`.
- A commit sequencer per database (the evolution of the actor) with concurrent snapshot
  readers.
- Multi-statement `;` scripts, `BEGIN`/`COMMIT`/`ROLLBACK`, `engine.transaction`, `engine.transaction_with_retry`
  and `Error::TxConflict`.
- `Engine::connect("mem://" | "rocksdb://...")`.
- Config keys `storage.url`, `storage.sync`, `transaction.isolation`, `query.timeout`
  and the cache sizes.
- A one-way importer from Phase 0 checkpoints.

**Crates.** `atlantis-kv` (new), `atlantis-storage`, `atlantis-vector`, `atlantis-engine`,
`atlantis-db`, `atlantis-core`.

**Docs.** 04, 05, 16, 21, 22.

**Exit criteria.** The Phase 0 integration suite passes unchanged on both backends.
Crash tests (kill during commit) never expose partial transactions under
`storage.sync = every`. Concurrent conflicting writers get `TxConflict`, and no lost
update is observable.

**Testing and benchmarks.** Write a backend conformance suite that runs once per backend.
Add a Jepsen-style randomized history checker for snapshot isolation and
serializability. Add fault-injection tests for fsync and kill. Benchmark commit
throughput and latency per sync mode, snapshot read scaling with reader count, and the
Phase 0 graph benchmarks to bound the regression.

### Phase 2 — Unified record and document/relational model

**Goal.** Make every node, edge and document a record in a table, with schema, indexes
and joins.

**Deliverables.**

- Namespaces, databases and tables (`CREATE NAMESPACE`, `CREATE DATABASE`, `USE`).
  Unlabeled nodes go into the reserved table `_default`.
- `RecordId` and the extended `Value` types, including `Geometry`.
- `id(n)` and `rid()`.
- Explicit keys on `CREATE`, with ULID as the default.
- Edge tables with `in`/`out`.
- Schema DDL (`SCHEMAFULL`, fields, types, `ASSERT`, `DEFAULT`, `COMPUTED`) with
  `Error::SchemaViolation`.
- The new crate `atlantis-index`: B-tree, unique and composite indexes, background builds
  (`index.workers`), and the Neo4j 5 `CREATE INDEX` / `CREATE CONSTRAINT` forms.
- Nested document access and updates, map literals as `Object` values, and `record<T>`
  links that dereference on access.
- A cost-based optimizer with the physical operators `IndexSeek`, `IndexRangeScan` and
  `KeyRangeScan`, plus join operators.
- Record helpers (`select`, `create`, `update`, `merge`, `delete`, `relate`),
  `use_ns`/`use_db` and `set_var`/`unset_var`.
- The renamed error type `atlantis_db::Error`.

**Crates.** `atlantis-core`, `atlantis-index` (new), `atlantis-cypher`, `atlantis-engine`,
`atlantis-storage`, `atlantis-db`.

**Docs.** 03, 06, 07, 08, 15, 21, 23.

**Exit criteria.** Document and relational recipes in the cookbook pass. Schemafull
tables reject invalid writes. `EXPLAIN` shows index use for equality and range
predicates and a join operator for link and value joins. A TPC-H-style subset (scale
factor 1) runs to completion.

**Testing and benchmarks.** Add proptest for value encoding order (memcomparable) and for
schema validation. Differentially test aggregation and joins against SQLite on generated
data. Benchmark index build throughput, point and range lookups, and join queries before
and after the optimizer.

### Phase 3 — Search and specialized models

**Goal.** Add full-text, geospatial, key-value and time-series data, and make vector
search first-class.

**Deliverables.**

- The new crate `atlantis-text`: analyzers, BM25, `CREATE FULLTEXT INDEX`,
  `db.text.search` and `text.highlight`.
- `db.search.hybrid` with reciprocal rank fusion.
- The new crate `atlantis-geo`: geo functions (`point.distance`, `geo.within`,
  `geo.intersects`) and `CREATE POINT INDEX`.
- KV: `db.kv.*`, `engine.kv()` and `tx.kv()`, TTL with a background expiry worker.
- Time-series tables (`TYPE TIMESERIES KEY ... RETENTION`), `ts.bucket` and retention
  workers.
- Vector index DDL with per-index options.
- `db.vector.search(indexName, ...)`, pre- and post-filtering, and quantization.
- Graph paths and algorithms: `shortestPath`, `allShortestPaths`, `algo.pagerank`,
  `algo.wcc`.
- The physical operators `VectorTopK`, `TextSearch` and `SpatialScan`.

**Crates.** `atlantis-text` (new), `atlantis-geo` (new), `atlantis-vector`, `atlantis-index`,
`atlantis-cypher`, `atlantis-engine`, `atlantis-db`.

**Docs.** 09, 10, 11, 12, 13, 15, 23, 24.

**Exit criteria.** Every Phase 3 recipe in the cookbook and every Phase 3 pattern in
[24 — RAG Patterns](24-rag-patterns.md) passes. Vector recall@10 is at least 0.95 on a
standard dataset at the default parameters. Expired KV keys and out-of-retention points
disappear within one worker interval.

**Testing and benchmarks.** Compare BM25 rankings against a reference implementation on
a fixed corpus. Check geo predicates against a reference geometry library. Benchmark
ANN with ann-benchmarks-style recall/QPS curves, hybrid retrieval latency, time-series
ingest rate and bucketed range queries, and graph algorithms on a public graph dataset.

### Phase 4 — Reactive

**Goal.** Let applications react to data changes durably and in real time.

**Deliverables.**

- Change feeds (`ALTER TABLE ... CHANGEFEED`, `db.changes`) with GC.
- Events (`CREATE EVENT`), running synchronously inside the triggering transaction.
- `LIVE MATCH` / `KILL LIVE` and the `engine.live` stream, emitted after commit.
- A bounded queue per live query (`live.queue_size`). Overflow kills the live query.

**Crates.** `atlantis-engine`, `atlantis-cypher`, `atlantis-storage`, `atlantis-db`.

**Docs.** 14, 21, 22, 23, 24.

**Exit criteria.** A failing event aborts its transaction. Rolled-back transactions
produce no notifications or change-feed entries. A client that falls behind is killed
and can resume from `db.changes` without gaps.

**Testing and benchmarks.** Add property tests that change feeds replay to the same
state, and stress tests with slow consumers. Benchmark notification latency
(commit → delivery) and write throughput with N live queries registered.

### Phase 5 — Server and security

**Goal.** Run Atlantis DB as a secure, multi-tenant network service.

**Deliverables.**

- The new crate `atlantis-server` and the `atlantis` binary (`start`, `shell`,
  `import`, `export`, `backup`, `restore`, `upgrade`, `version`, `isready`).
- HTTP endpoints and WebSocket RPC on port 7700.
- `atlantis.toml` and `ATLANTIS_*` environment variables.
- The new crate `atlantis-auth`: system users and roles (`owner`, `editor`, `viewer`),
  `CREATE ACCESS` record access, JWT, Argon2id, table `PERMISSIONS` (default `NONE` for
  record users), an audit log, `--insecure-no-auth` for loopback only, and `signin`,
  `signup`, `authenticate`, `invalidate`.

**Crates.** `atlantis-server` (new), `atlantis-auth` (new), `atlantis-engine`, `atlantis-db`.

**Docs.** 17, 18, 22, 23, 24.

**Exit criteria.** The cookbook suite passes over HTTP and WebSocket RPC. Permission
tests show that a record user cannot read or write outside its `PERMISSIONS`, including
through search procedures and live queries. Backup → restore round-trips a database
byte-for-byte at the logical level.

**Testing and benchmarks.** Write protocol conformance tests from the RPC method list,
plus fuzzing of the HTTP and RPC decoders. Add an authorization test matrix (role ×
operation × level). Benchmark request throughput and p99 latency over HTTP and
WebSocket against the embedded baseline.

### Phase 6 — SDKs

**Goal.** Offer the same method surface in every main language.

**Deliverables.**

- Remote Rust (`Engine::connect("ws://...")`, cargo features `protocol-ws` and
  `protocol-http`).
- SDKs for Python (`atlantisdb`), JavaScript/TypeScript (`atlantisdb.js`) with an
  embedded WASM build, Go and Java.
- An optional Bolt listener (feature `bolt`, port 7687).

**Crates.** `atlantis-db`, `atlantis-server`. SDKs live in their own packages.

**Docs.** 19, 21.

**Exit criteria.** A shared, language-neutral conformance suite passes in every SDK.
Neo4j drivers can run the supported Cypher subset over Bolt.

**Testing and benchmarks.** Run the conformance suite in CI per SDK against a live
server. Measure WASM bundle size and embedded performance in the browser.

### Phase 7 — Distributed

**Goal.** Scale horizontally with stateless compute nodes over TiKV.

**Deliverables.**

- The `kv-tikv` backend and `tikv://` URLs.
- Node registration and heartbeats in `/sys/cluster` (`cluster.heartbeat_interval`,
  `cluster.heartbeat_expiry`).
- Live queries registered in KV, with fan-out through per-node notification queues.
- HNSW and full-text indexes kept in KV, with lazily rebuilt per-node caches checked by
  version.
- Ops tooling: health, metrics and rolling upgrade.

**Crates.** `atlantis-kv`, `atlantis-server`, `atlantis-engine`, `atlantis-vector`,
`atlantis-text`.

**Docs.** 20, 22.

**Exit criteria.** The backend conformance suite and the full cookbook pass on a
three-node TiKV cluster. Killing a compute node loses no committed data, and clients
reconnect to another node. Live queries on a dead node are cleaned up within the
heartbeat expiry window.

**Testing and benchmarks.** Run chaos tests (node kill, network partition) with the
Phase 1 history checker. Benchmark throughput scaling with compute node count and
cross-region commit latency.

### Benchmark strategy across phases

Each phase records its benchmarks against the previous phase's baseline in the same
Criterion suites in `benches/`. A regression of more than 10% on an earlier benchmark
needs an explicit justification in the phase's plan before the phase exits.

## Checklist

- [x] Requirements and architecture design
- [x] Core types and property graph model (`atlantis-core`)
- [x] Hot/cold tiered storage, CSR+delta adjacency (`atlantis-storage`)
- [x] HNSW index algorithm (`atlantis-vector`, persistence pending)
- [x] Cypher lexer, parser and planner skeleton (`atlantis-cypher`)
- [x] Single-writer actor runtime (`atlantis-engine`)
- [x] Multi-model documentation set (this docs restructure)
- [ ] Phase 0 — Complete graph + vector core
- [ ] Phase 1 — Transactional storage foundation
- [ ] Phase 2 — Unified record and document/relational model
- [ ] Phase 3 — Search and specialized models
- [ ] Phase 4 — Reactive
- [ ] Phase 5 — Server and security
- [ ] Phase 6 — SDKs
- [ ] Phase 7 — Distributed

## Design Decisions

- **Correctness before foundations.** Phase 0 fixes today's engine first, so its
  integration suite becomes the regression oracle for the Phase 1 storage migration.
- **KV before models.** Every later model is a key layout plus an index over
  `atlantis-kv`. Building the transactional layer first avoids reworking each model for
  ACID later.
- **Server after the data model.** Protocols expose the record model and permissions;
  stabilising them first keeps the RPC surface from churning.
- **Distributed last, but designed in from Phase 1.** The `KvStore` trait and stateless
  engine make TiKV a backend swap rather than a rewrite.

## What's Next

- [02 — Architecture](02-architecture.md) — the target system these phases build.
- [23 — Query Cookbook](23-query-cookbook.md) — recipes that serve as phase exit tests.
- [README](README.md) — chapter index and status.
