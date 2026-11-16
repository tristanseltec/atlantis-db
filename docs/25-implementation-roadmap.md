# 25 — Implementation Roadmap

> **Status:** Partially implemented — Phase 0 is essentially complete, Phase 1 is mostly done (the `rocksdb` backend is open), Phase 2 is partial; Phases 3–7 are planned (this chapter records current status and the phased plan)
> **Crates:** all workspace crates; `atlantis-index`, `atlantis-text`, `atlantis-geo`, `atlantis-auth`, `atlantis-server` (planned)

This chapter has two parts. **Current status** records what the code in `crates/`
does today, verified against the source. **Phased roadmap** is the plan that takes
Atlantis DB from a pre-release embedded graph + vector engine to the multi-model
database the other chapters describe. Every "Planned — Phase N" marker elsewhere in the
docs refers to a phase defined here.

## Current status

### Progress

| Item | State |
|---|---|
| Phase 0 — graph + vector core | Essentially complete. Cypher runs on the record store; every parser and executor gap listed for Phase 0 is fixed and pinned by integration tests; vector indexes are populated and derived from records. Not done, and moved: `shortestPath`/`allShortestPaths` and `algo.*` stay in Phase 3; HNSW persistence is replaced by rebuilding indexes from records (persistence in `vx/` keys stays open, below); the Criterion baseline is not recorded. |
| Phase 1 — transactional storage | Mostly done. Done: the `atlantis-kv` crate, memcomparable keys, MVCC with snapshot and serializable isolation, the `mem` backend with a write-ahead log (`SyncMode::Every`/`Never`), crash recovery and periodic checkpoints, one engine per directory (`LOCK`), record transactions on `Engine`, Cypher on KV (the legacy graph store is deleted), multi-statement scripts with `BEGIN`/`COMMIT`/`ROLLBACK`, and Cypher inside Rust transactions. Also done: group commit, the `interval` sync mode and `transaction.isolation`. Open: the `rocksdb` backend and backend features, the per-database commit sequencer, the adjacency cache, HNSW in `vx/` keys, `Engine::connect`, config keys, query workers off the async executor and `query.timeout`. The importer from legacy checkpoints is dropped: opening a legacy `checkpoint.db` fails with an error. |
| Phase 2 — unified record model | Partial. Done: `RecordId`/`RecordKey`, `Record`, `Edge`, the extended `Value` types except `Decimal` and `Range`, namespaces and databases (`DatabaseRef`, `use_db` for Cypher and records), the table catalog with implicit schemaless tables, edge tables with endpoint restrictions, `_default`, the secondary label index, nested document access and updates, map literals as `Object`, record links that dereference, `id(n)`/`rid()`, explicit keys through Cypher `{id: ...}`. Open: schema DDL, fields and assertions, `atlantis-index` B-tree indexes and constraints, `CREATE NAMESPACE`/`CREATE DATABASE`/`USE`, the planner and cost-based optimizer, `EXPLAIN`, and engine-level record helpers. |
| Phase 3 — search | Partial. Done: `CREATE VECTOR INDEX` / `DROP INDEX` with per-index options, `db.vector.search(indexName, ...)` with threshold and `efSearch`, filtered vector search with `WHERE` push-down (F3), `point()` and `point.distance`. Open: everything else in Phase 3. |

**One store.** `Engine::execute` (Cypher), `Transaction::execute` and the record API
read and write the same records. The legacy `GraphStore` (hot/cold tier, CSR + delta,
LRU tiering, `checkpoint.db`) is deleted.

**Known limitations.**

- The `async` `Engine` methods do their work synchronously on the calling task: query
  execution, the WAL fsync, and the first-search build of a vector index, which runs
  under that index's write lock. Heavy calls block a tokio worker; use
  `tokio::task::spawn_blocking` or a dedicated runtime for large workloads. Moving the
  work off the async executor is planned (Phase 1).
- There are no per-query row or time limits. Unbounded variable-length patterns and
  huge `UNWIND`s can use a lot of memory or time; `range()` is capped at 10 million
  items. `query.timeout` remains planned (Phase 1).
- A failed Cypher statement inside a Rust transaction poisons it; there are no
  savepoints.
- Not implemented and not scheduled in a phase yet: map projections, `EXISTS { }` and
  `CALL { }` subqueries, `FOREACH`, `LOAD CSV`.

### Build and tests

The workspace compiles and `cargo test --workspace` passes. The integration tests in
`crates/atlantis-db/tests/cypher.rs` and `records.rs` are the authoritative list of
behaviour that works end to end. `benches/benches/` holds Criterion benchmarks for
storage, Cypher queries and vector search; no baseline is recorded in the repository.

### Per crate

**`atlantis-core`.** The record model: `DatabaseRef`, `RecordId`/`RecordKey`, `Record`,
`Edge`, `Object`, and identifier validation. `Value` with the extended variants
(`Bytes`, `Datetime`, `Duration`, `Uuid`, `Object`, `Geometry`, `RecordId`), but not
`Decimal` or `Range`. `Direction`, `NodeId` (the HNSW entry id), `EngineConfig` with
builder and validation, `SyncMode`, and `AtlantisError` with an `Error` alias. The
legacy `Node`, `Relationship`, `Label`, `RelType` and `PropertyMap` types are removed.
See [03 — Data Model](03-data-model.md).

**`atlantis-kv`.** The `KvStore`/`KvTransaction` traits (synchronous), the memcomparable
encoding, the key layout, and `MemStore`: MVCC, first-committer-wins conflicts,
serializable read validation, version pruning, the write-ahead log with crash recovery,
and atomic checkpoints. There is no `rocksdb` or `tikv` backend and no Cargo backend
features. See [04 — Storage Engine](04-storage-engine.md).

**`atlantis-storage`.** `RecordStore`/`RecordTx`: records, edges with adjacency entries
on both endpoints, secondary labels (`set_labels`), the table catalog, vector index
definitions in the catalog, and `CommitInfo` with the changed record ids. See
[04 — Storage Engine](04-storage-engine.md).

**`atlantis-vector`.** HNSW insert, search, threshold search and tombstone delete;
cosine, Euclidean and dot-product distances; a brute-force recall test. The index is
in memory; the engine rebuilds it from records.

**`atlantis-cypher`.** A hand-written lexer and recursive-descent parser (`parse`,
`parse_script`) producing the AST the engine interprets. There is no planner. See
[06 — Query Language](06-query-language.md).

**`atlantis-engine`.** `Datastore`: opens the store and checkpoint thread, runs scripts
clause by clause against record transactions, and keeps vector indexes derived from
records. See [15 — Query Planner & Executor](15-query-planner-executor.md) and
[16 — Engine Runtime](16-engine-runtime.md).

**`atlantis-db`.** `Engine`, `Transaction`, `Config`, `ResultSet`, `Row` and `Params`,
plus re-exports of the record and transaction types (see
[21 — API Reference](21-api-reference.md)).

### Chapter status

| Chapter | Status | Completed by |
|---|---|---|
| [01](01-getting-started.md), [02](02-architecture.md), [03](03-data-model.md), [04](04-storage-engine.md), [06](06-query-language.md), [15](15-query-planner-executor.md), [16](16-engine-runtime.md) | Partial | Phases 1–2, then later phases for platform parts |
| [05](05-transactions-consistency.md) | Partial | Phase 1 (Phase 4 for change feeds) |
| [07](07-schema-ddl.md), [08](08-document-relational.md) | Partial | Phase 2 |
| [09](09-graph-model.md), [10](10-vector-index.md) | Partial | Phase 3 (shortest paths, algorithms, vector persistence and filtering) |
| [11](11-full-text-search.md), [12](12-geospatial.md), [13](13-key-value-time-series.md) | Phase 3 | Phase 3 |
| [14](14-live-queries-events.md) | Phase 4 | Phase 4 |
| [17](17-server-protocols.md), [18](18-security-auth.md) | Phase 5 | Phase 5 |
| [19](19-client-sdks.md) | Phase 6 | Phase 6 |
| [20](20-distributed-architecture.md) | Phase 7 | Phase 7 |
| [21](21-api-reference.md), [22](22-configuration.md), [23](23-query-cookbook.md), [24](24-rag-patterns.md) | Partial | Grow with every phase |

## Priorities

> Set 2026-11-16. Rationale and targets: [26 — Positioning & Performance Targets](26-positioning-and-targets.md).
> Design: [02 — Architecture § Hybrid Retrieval Path](02-architecture.md#hybrid-retrieval-path-focus).

The phases below stay the map, but work follows a **focus track** that cuts across
them: the transactional hybrid retrieval path. In order:

| # | Step | Draws from | Exit measure |
|---|---|---|---|
| F1 | **Atlantis Retrieval Bench**: dataset generator, exact filtered ground truth, adapters for SurrealDB (embedded), LadybugDB, FalkorDB, Neo4j and Qdrant + Neo4j; record the Atlantis baseline | — | Baseline committed; every later step reports against it. *In progress:* harness, corpus, ground truth, workloads and the Atlantis adapter are in [`benches/retrieval`](../benches/retrieval/README.md) with baselines in `benches/results/`; competitor adapters are open. |
| F2 | **Vector core**: *done:* the HNSW connectivity bug the bench found (directed edges, neighbour-selection heuristic, insert descent, tombstones as waypoints; recall@10 0.22 → 1.00, see [26 § Retrieval Bench Baseline](26-positioning-and-targets.md#retrieval-bench-baseline-2026-11-16)); precomputed norms, contiguous `f32` arena, flat layer-0 links, epoch visited sets, AVX2/FMA distances with runtime dispatch, per-index read-write locks (at 20k × 768: build 186 s → 21 s, top-10 p50 1.87 → 0.27 ms at recall 1.00, 8-thread graph-RAG 225 → 3,041 QPS); parallel index build with lock-free link reads (`insert_batch`, `vector.build_threads`; build 21 s → 5.0 s at 20k, 27 s at 100k × 768 with recall 1.00). Open: AVX-512 and NEON kernels, search on a query worker pool, SQ8 with re-rank, the 1M-scale measurement | Phase 1, Phase 3 | Unfiltered top-10 over 1M × 768 ≤ 2 ms p50 at recall@10 ≥ 0.95 |
| F3 | **Filtered search**: *done:* allow-set bitmaps, exact and filtered-traversal strategies chosen per query by a cost estimate, per-field columns with value maps kept current by commits, `WHERE`-after-`YIELD` push-down of field comparisons (filtered recall@10 0.09 → 1.00 at 1% selectivity, p50 0.15 ms at 20k × 768). Open: push-down of labels, nested fields and `OR`; property indexes to replace columns; measurement at 1M | Phase 2 (planner), Phase 3 | recall@10 ≥ 0.95 at 1% selectivity |
| F4 | **Consistent search**: versioned index entries, transaction overlay (read-your-writes), index image at checkpoint | Phase 1, Phase 3 | A write is searchable in its own transaction; open of 1M vectors < 1 s |
| F5 | **BM25 and fusion**: `atlantis-text`, `db.text.search`, `db.search.hybrid` with RRF, on the same snapshot | Phase 3 | Hybrid p50 / p99 ≤ 3 ms / ≤ 8 ms under mixed load |
| F6 | **Group commit**: *done* in `MemStore` ahead of the sequencer (single-record durable commits 204/s → 6,198/s at 64 writers on NVMe; readers under write load 801 → 4,695 QPS), with the `interval` sync mode and `transaction.isolation`. Open: the per-database sequencer; 50,000/s needs multi-record transactions or index maintenance off the commit path (F4) | Phase 1 | Durable ingest ≥ 50,000 records/s |
| F7 | **`atlantis-py`**: embedded Python package (PyO3, NumPy buffers, GIL released) | Ahead of Phase 6 | The RAG patterns in [24](24-rag-patterns.md) run from Python |
| F8 | **Publish** the benchmark and results | — | Reproducible by a third party |

The largest risk is F2: today's search is 5–10x off comparable HNSW libraries, so it is
prototyped first after the harness.

**Commerce stages** run alongside and after the focus track
([27 — Commerce](27-commerce.md#stages)): C0 (dataset and labelled queries, with
F1–F5), C1 read path (`Decimal`, bitmap facets, multi-vector fusion, unique
constraints, read-only `atlantis-mcp`), C2 write path (escrow reservations, quotes,
connectors, change feeds), C3 agent protocols and buyer memory, C4 design partners.
Engine features they need land in the open core; the commercial crates are described in
[02 — Architecture § Editions](02-architecture.md#editions-and-extension-points-planned).

**Deferred (designed, unscheduled):** `atlantis-geo` and spatial indexes, the
key-value API, time-series tables, the Go and Java SDKs, Bolt, and Phase 7. Their
chapters stay as design; they are off the critical path until the focus track exits.

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

**Goal.** Make the embedded engine correct and usable.

**State.** Essentially complete. The work was done by moving Cypher onto the record
store (Phase 1) instead of fixing the legacy graph store, which was deleted. Every item
below is implemented and covered by `crates/atlantis-db/tests/cypher.rs` unless marked
otherwise.

**Deliverables.**

1. A green build, including the formerly failing Cypher and HNSW unit tests.
2. Correct graph semantics: multi-hop `MATCH`, rows kept across several `MATCH`
   clauses, `<-`, `--`, `-[]-` and comma-separated patterns, untyped and alternative
   relationship types, inline property maps, variable-length `*min..max`, path variables
   and `length`, `nodes`, `relationships`.
3. Writes: `CREATE` relationships and paths, `MATCH ... CREATE`, every created row
   returned, `SET` (including `n.a.b`, `n = map`, `n += map` and labels), `REMOVE`,
   `MERGE` with `ON CREATE` / `ON MATCH`, `DELETE` and `DETACH DELETE` that remove
   records and edges.
4. Query features: leading `RETURN` and `UNWIND`; `UNION` / `UNION ALL`; `CASE`; list
   comprehensions; `ASC`/`DESC`; `SKIP`/`LIMIT` in either order; `ORDER BY`, `LIMIT`
   and `DISTINCT` on `WITH`; trailing text after `RETURN` is a parse error; `ORDER BY`
   on any expression; `OPTIONAL MATCH` null rows; aggregation with implicit grouping and
   `count(*)`; `<>`, `IN`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `=~`, `IS NULL`,
   `XOR`, `%`; map literals; scalar functions; dotted function names.
5. Vector: indexes populated from records and updated on create, update and delete;
   `db.vector.search` by index name (or a label covered by one index), `k` as a
   parameter, yielding the record. Moved: persistence is replaced by rebuilding from
   records, with KV persistence in `vx/` keys left open in Phase 1.
6. Storage and runtime: ids are stable record ids; periodic checkpoints driven by
   `checkpoint_interval_secs`; the full `StorageConfig` reaches the store.
   `memory_budget_bytes` and eviction were removed with the legacy store.

**Not done.** `shortestPath`/`allShortestPaths` and graph algorithms stay in Phase 3.
The Criterion baseline and the proptest round-trips are not recorded.

**Crates.** `atlantis-core`, `atlantis-storage`, `atlantis-vector`, `atlantis-cypher`,
`atlantis-engine`, `atlantis-db`.

### Phase 1 — Transactional storage foundation

**Goal.** Replace the hot/cold tier with an ordered, transactional KV layer, so every
model shares ACID storage.

**Deliverables.** Items marked *done* are implemented; the rest are open.

- *Done:* the new crate `atlantis-kv`, with synchronous `KvStore`/`KvTransaction`
  traits (`begin`, `get`, `set`, `delete`, `scan`, `commit`, `cancel`) and the
  memcomparable key encoding from [04 — Storage Engine](04-storage-engine.md).
- *Done:* MVCC with snapshot isolation by default and optional serializable isolation.
- *Done:* the `mem` backend with a write-ahead log, sync modes `every`, `interval` and
  `never`, group commit (fsync outside the store lock, shared by concurrent commits,
  durable commits published in version order), idempotent catalog writes, crash
  recovery, and periodic and explicit checkpoints. Open: the `rocksdb` backend and the
  Cargo features `kv-mem` and `kv-rocksdb`.
- *Done:* Cypher on KV; the legacy graph store is deleted. Open: an adjacency cache
  rebuilt from `g/` keys, and HNSW persisted under `vx/`.
- A commit sequencer per database, replacing the store-wide commit lock, and query work
  moved off the async executor. Snapshot readers already run concurrently.
- *Done:* `engine.transaction`, `engine.transaction_with_isolation`,
  `engine.transaction_with_retry`, `engine.read`, `Error::TxConflict`, multi-statement
  `;` scripts, `BEGIN`/`COMMIT`/`ROLLBACK`, and Cypher inside transactions
  (`Transaction::execute`).
- `Engine::connect("mem://" | "rocksdb://...")`.
- Config keys `storage.url`, `query.timeout` and the cache sizes. *Done:*
  `StorageConfig::sync` (with `Interval`), `transaction.isolation`
  (`TransactionConfig`) and `checkpoint_interval_secs` in `EngineConfig`.
- *Dropped:* the importer from legacy checkpoints. Opening a directory with only a
  legacy `checkpoint.db` fails with an error.

**Crates.** `atlantis-kv` (new), `atlantis-storage`, `atlantis-vector`, `atlantis-engine`,
`atlantis-db`, `atlantis-core`.

**Docs.** 04, 05, 16, 21, 22.

**Exit criteria.** The Cypher integration suite passes unchanged on both backends.
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

**Deliverables.** Items marked *done* are implemented in the Rust record API; the rest
are open.

- *Done:* namespaces, databases and tables in storage (`DatabaseRef`, `use_db`, the
  table catalog, implicit schemaless tables, the reserved table `_default` for
  unlabeled nodes). Open: the Cypher forms `CREATE NAMESPACE`, `CREATE DATABASE` and
  `USE`.
- *Done:* `RecordId` and the extended `Value` types, including `Geometry`. Open:
  `Decimal` and `Range`.
- *Done:* `id(n)` and `rid()`.
- *Done:* explicit keys on record creation, with ULID as the default, also through
  Cypher `CREATE (:T {id: ...})`.
- *Done:* edge tables with `in`/`out` and endpoint-table restrictions, and a secondary
  label index.
- Schema DDL (`SCHEMAFULL`, fields, types, `ASSERT`, `DEFAULT`, `COMPUTED`) with
  `Error::SchemaViolation`.
- The new crate `atlantis-index`: B-tree, unique and composite indexes, background builds
  (`index.workers`), and the Neo4j 5 `CREATE INDEX` / `CREATE CONSTRAINT` forms.
- *Done:* nested document access and updates, map literals as `Object` values, and
  stored record links that dereference on access. Open: the typed `record<T>` field.
- A planner and cost-based optimizer with the physical operators `IndexSeek`, `IndexRangeScan` and
  `KeyRangeScan`, plus join operators.
- Record helpers (`select`, `create`, `update`, `merge`, `delete`, `relate`),
  `use_ns` and `set_var`/`unset_var`. `Transaction` already offers transaction-level
  record operations and Cypher, and `use_db(ns, db)` exists.
- *Done:* the error type `atlantis_db::Error` (an alias of `AtlantisError`).

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
- *Done:* vector index DDL with per-index options, and `db.vector.search(indexName, ...)`.
  *Done (F3):* filtered search with push-down from `YIELD ... WHERE`. Open: quantization.
- *Done:* `point()` and `point.distance` as functions.
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
- [x] HNSW index algorithm (`atlantis-vector`)
- [x] Multi-model documentation set (this docs restructure)
- [x] Green build (Phase 0 compile fixes)
- [x] Record foundation: `atlantis-kv` with the `mem` backend, the record store and
  record transactions (part of Phases 1 and 2)
- [x] Write-ahead log and crash recovery for the `mem` backend
- [x] Hand-written Cypher parser and clause interpreter on the record store; legacy
  hot/cold graph store removed
- [x] Phase 0 — Complete graph + vector core (shortest paths and algorithms moved to
  Phase 3)
- [ ] Phase 1 — Transactional storage foundation (mostly done; `rocksdb` open)
- [ ] Phase 2 — Unified record and document/relational model (partial)
- [ ] Phase 3 — Search and specialized models (vector index DDL done)
- [ ] Phase 4 — Reactive
- [ ] Phase 5 — Server and security
- [ ] Phase 6 — SDKs
- [ ] Phase 7 — Distributed

## Design Decisions

- **Correctness on the new foundation.** Phase 0 was delivered by moving Cypher onto
  the record store rather than fixing the legacy graph store; its integration suite is
  the regression oracle for the `rocksdb` backend and the planner.
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
