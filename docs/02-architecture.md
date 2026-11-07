# 02 — Architecture

> **Status:** Partially implemented — the seven-crate embedded engine exists: the Cypher graph engine (with Phase 0 gaps) and, beside it, the record store on `atlantis-kv` (`mem` backend); the rest of the layered multi-model architecture is Planned — Phases 0–7
> **Crates:** `atlantis-core`, `atlantis-kv`, `atlantis-storage`, `atlantis-index` (planned), `atlantis-vector`, `atlantis-text` (planned), `atlantis-geo` (planned), `atlantis-cypher`, `atlantis-engine`, `atlantis-auth` (planned), `atlantis-server` (planned), `atlantis-db`

This chapter describes how Atlantis DB is layered, which crate owns which
responsibility, how a request moves through the system, and how the same engine runs
embedded, as a server and as a cluster. The target design comes first. The
"Implemented Today" section at the end describes the code as it exists now.

## Target Architecture

Atlantis DB is one engine with one query language, **Atlantis Cypher**, over one record
model (see [Data Model](03-data-model.md)). Every data model (graph, document,
relational, vector, full-text, geospatial, key-value, time series) is stored as keys in
a single transactional key-value layer. Specialised indexes sit between the query
engine and that layer.

```
 Clients: Rust embedded | SDKs (Rust/Python/JS/Go/Java) | CLI | HTTP | WS RPC | Bolt
                                │
 atlantis-server (planned) ── protocols, sessions, auth handshake
                                │
 atlantis-engine ── session, transactions, permissions enforcement,
                    executor, live-query & event dispatch
                                │
 atlantis-cypher ── lexer → parser → AST → logical plan → cost-based optimizer
                                │
 Index layer: atlantis-index (B-tree / unique / composite)   atlantis-vector (HNSW)
              atlantis-text (full-text, BM25)                atlantis-geo (spatial)
                                │
 atlantis-storage ── record store, edge store, CSR adjacency cache, tiering/caching
                                │
 atlantis-kv ── transactional ordered KV trait, key encoding, MVCC
     backends: mem | rocksdb (default persistent) | tikv (distributed)
                                │
 atlantis-core ── Value, RecordId, Geometry, Datetime, errors, config
```

### Layers

| Layer | Responsibility | Owning chapter |
|---|---|---|
| Clients | Embedded Rust API, SDKs, CLI, raw protocols | [Client SDKs](19-client-sdks.md) |
| Server | Listeners, protocol framing, sessions, sign-in handshake | [Server & Protocols](17-server-protocols.md) |
| Engine | Sessions, transactions, permission checks, execution, live-query and event dispatch | [Engine Runtime](16-engine-runtime.md) |
| Query | Parsing, planning and cost-based optimization of Atlantis Cypher | [Query Language](06-query-language.md), [Query Planner & Executor](15-query-planner-executor.md) |
| Indexes | Secondary, vector, full-text and spatial indexes, all persisted as KV entries | [Schema & DDL](07-schema-ddl.md), [Vector Index](10-vector-index.md), [Full-Text Search](11-full-text-search.md), [Geospatial](12-geospatial.md) |
| Storage | Record and edge stores, adjacency cache, cache management | [Storage Engine](04-storage-engine.md) |
| KV | Ordered transactional key space, MVCC, pluggable backends | [Storage Engine](04-storage-engine.md), [Transactions & Consistency](05-transactions-consistency.md) |
| Core | Shared types, errors, configuration | [Data Model](03-data-model.md), [Configuration](22-configuration.md) |

### Crate Map

| Crate | Role | Status |
|---|---|---|
| `atlantis-core` | Types: `Value`, `RecordId`, `Geometry`, `Datetime`, `Duration`, errors, config | Partial (record and graph types implemented; `Decimal`, `Range` Phase 2) |
| `atlantis-kv` | `KvStore` / `KvTransaction` traits, key encoding, MVCC; features `kv-mem`, `kv-rocksdb`, `kv-tikv` | Partial (traits, encoding, MVCC and the `mem` backend implemented; `rocksdb` and backend features Phase 1, `tikv` Phase 7) |
| `atlantis-storage` | Record and edge stores on top of `atlantis-kv`; CSR+delta becomes an adjacency cache; tiering becomes cache management | Partial (record store on `atlantis-kv` implemented; Cypher still uses the hot/cold `GraphStore`; adjacency cache Phase 1) |
| `atlantis-index` (planned) | Secondary B-tree, unique and composite indexes; index manager and background index builds | Phase 2 |
| `atlantis-vector` | HNSW, persisted through `atlantis-kv` | Partial (in-memory HNSW today; persistence Phase 0/1) |
| `atlantis-text` (planned) | Analyzers, inverted index, BM25 | Phase 3 |
| `atlantis-geo` (planned) | Geometry operations, spatial index (geohash / S2-style cells over KV) | Phase 3 |
| `atlantis-cypher` | Lexer, parser, planner; dialect extensions; cost-based optimizer | Partial (Cypher subset today; extensions Phases 1–5) |
| `atlantis-engine` | Execution, transactions, permissions, live queries, events, change feeds | Partial (single-writer actor today) |
| `atlantis-auth` (planned) | Users, roles, access methods, JWT, Argon2id password hashing | Phase 5 |
| `atlantis-server` (planned) | HTTP, WebSocket RPC, optional Bolt; the `atlantis` binary | Phase 5 (Bolt: Phase 6) |
| `atlantis-db` | Public facade; one `Engine` API for embedded and remote connections | Partial (embedded Cypher and record transactions today; remote Phase 6) |

### Dependency Rules

- Dependencies point down the layer diagram. A crate never depends on a crate above it.
- `atlantis-core` has no internal dependencies.
- Index crates and `atlantis-storage` reach the disk only through `atlantis-kv`, so a
  new backend (for example TiKV) needs no changes above the KV layer.
- `atlantis-cypher` depends only on `atlantis-core`. It produces plans; it never reads
  data. `atlantis-engine` connects plans to storage and indexes.
- `atlantis-server` and `atlantis-auth` sit above the engine. The embedded build does
  not compile them.
- Users depend on `atlantis-db` only.

## Deployment Modes

The same engine runs in three shapes. Only the connection URL changes, because every
mode shares the `atlantis-db` `Engine` API and the RPC method surface.

| Mode | What runs | Storage backend | Connection URL | Status |
|---|---|---|---|---|
| **Embedded** | The engine as a library inside the application process | `mem` or `rocksdb` | `mem://`, `rocksdb://path` | Partial (in-memory and file path today; URLs Phase 1) |
| **Server** | One `atlantis start` process serving HTTP, WebSocket RPC and optionally Bolt | `rocksdb` (or `mem`) | `ws://host:7700`, `wss://`, `http://`, `https://` | Phase 5 (Bolt: Phase 6) |
| **Cluster** | Many stateless `atlantis` compute nodes over a TiKV cluster | `tikv` | `tikv://pd1:2379,pd2:2379` (node side); clients use `ws://` / `http://` | Phase 7 |

```
 Embedded                 Server                         Cluster
 ┌──────────────┐         ┌──────────────┐               ┌─────────┐ ┌─────────┐
 │ application  │         │ clients      │               │ clients │ │ clients │
 │  └─ engine   │         └──────┬───────┘               └────┬────┘ └────┬────┘
 │     └─ kv    │                │ :7700                      │ :7700     │
 │       (mem / │         ┌──────▼───────┐               ┌────▼────┐ ┌────▼────┐
 │      rocksdb)│         │ atlantis     │               │atlantis │ │atlantis │ stateless
 └──────────────┘         │  └─ engine   │               └────┬────┘ └────┬────┘
                          │     └─ kv    │                    └─────┬─────┘
                          │   (rocksdb)  │                    ┌─────▼─────┐
                          └──────────────┘                    │   TiKV    │ Raft
                                                              └───────────┘
```

- **Embedded** is the default for applications, agents and tests. There is no network
  hop and no authentication layer; the application is trusted.
- **Server** adds protocols, authentication, permissions and multi-tenant namespaces.
  Default port **7700**; Bolt (feature `bolt`) listens on **7687**.
- **Cluster** separates compute from storage. Compute nodes keep only caches (adjacency,
  HNSW, full-text) and rebuild them lazily. See
  [Distributed Architecture](20-distributed-architecture.md).

## Request Flow (Target)

A query sent to a server takes this path. An embedded call starts at step 3.

```
client ── POST /query or WS {method: "query"} ──────────────────────────────┐
                                                                            │
 1. atlantis-server   decode request, resolve Atlantis-NS / Atlantis-DB,    │
                      verify bearer token → session ($auth, $session)       │  Phase 5
 2. atlantis-auth     check system-user role or record-access identity      │
                                                                            │
 3. atlantis-engine   open a transaction: snapshot for reads               │  Phase 1
 4. atlantis-cypher   tokenize → parse → plan → cost-based optimize         │  Phase 2
                      (index selection, join order)                         │
 5. atlantis-engine   execute operators; apply table PERMISSIONS as         │  Phase 5
                      row filters; evaluate ASSERT / DEFAULT / COMPUTED     │  Phase 2
 6. indexes/storage   index lookups and record reads on the snapshot;       │
                      writes buffered in the transaction                    │
 7. atlantis-kv       commit through the database's commit sequencer;       │  Phase 1
                      conflict → Error::TxConflict (client may retry)       │
 8. atlantis-engine   after commit: append change feed, fire events,        │  Phase 4
                      push LIVE notifications                               │
 9. response          rows (JSON or CBOR) back to the client ◄──────────────┘
```

### Concurrency Model (Phase 1)

Today the record store already runs optimistic MVCC transactions concurrently with the
Cypher actor, on the `mem` backend; commits are validated one at a time under a
store-wide lock. The commit sequencer below is still planned.

- **Writes**: each database has one **commit sequencer**, the evolution of today's
  single-writer actor. It orders commits and validates conflicts.
- **Reads**: run concurrently on MVCC snapshots and never wait for the sequencer.
- **Background workers**: index builds, adjacency-cache maintenance, change-feed
  retention and time-series downsampling run outside the request path.
- **Isolation**: snapshot isolation by default, serializable on request. See
  [Transactions & Consistency](05-transactions-consistency.md).

## Design Principles

1. **One language.** Atlantis Cypher follows Neo4j 5 / openCypher / GQL syntax wherever
   it exists and adds extensions only where Cypher has none. There is no second query
   language.
2. **One record model.** All data models are views over records in tables, so indexes,
   permissions, transactions and change feeds are implemented once.
3. **Everything is a key.** Records, adjacency, indexes, change feeds and system
   metadata share one ordered transactional key space. Backends are swappable behind
   `atlantis-kv`.
4. **Embedded first, same API everywhere.** The embedded `Engine` API is the reference.
   Server, SDKs and cluster expose the same operations.
5. **Caches are derived.** The CSR adjacency cache, HNSW graph and full-text structures
   in memory can always be rebuilt from KV, which keeps compute nodes stateless.
6. **Honest layering.** The planner does not read data, storage does not know Cypher,
   and the server does not execute queries.

## Implemented Today

The current code is an embedded engine in seven crates. It has **two stores that do
not share data yet**:

- the Cypher graph + vector engine (`Engine::execute`), on the hot/cold `GraphStore`;
- the record store (`Engine::transaction`, `Engine::read`), on `atlantis-kv` with the
  in-memory `mem` backend and a `records.kv` checkpoint file.

There is no server, authentication or schema yet.

### Crates and Dependencies

```
atlantis-core          ← no internal dependencies
    ↑
atlantis-kv            ← core
atlantis-vector        ← core
atlantis-cypher        ← core
    ↑
atlantis-storage       ← core, kv
    ↑
atlantis-engine        ← core, storage, vector, cypher
    ↑
atlantis-db            ← core, engine, kv, storage (public facade)
```

| Crate | Responsibility today |
|---|---|
| `atlantis-core` | `Value` (with the extended types), record types (`DatabaseRef`, `RecordId`, `Record`, `Edge`), graph types (`Node`, `Relationship`, `Label`, `RelType`, `PropertyMap`), `Direction`, config, `AtlantisError` |
| `atlantis-kv` | `KvStore` / `KvTransaction` traits, memcomparable key encoding and layout, `MemStore` MVCC backend with checkpoint file |
| `atlantis-storage` | `RecordStore` / `RecordTx` on `atlantis-kv`; `GraphStore` for Cypher: in-memory hot tier (CSR + delta adjacency), append-only on-disk cold tier, LRU tiering manager, bincode checkpoint |
| `atlantis-vector` | In-memory HNSW index and distance metrics; persistence is a stub |
| `atlantis-cypher` | Hand-written lexer and recursive-descent parser, logical planner; the optimizer is a pass-through |
| `atlantis-engine` | `CoreEngineActor` on tokio, command protocol, `EngineHandle`, query executor |
| `atlantis-db` | `Engine` (Cypher and record transactions), `Config`, `Params`, `ResultSet`, `Row`, re-exported value, record and transaction types |

### Query Flow Today

`Engine::execute(cypher, params)` sends a `Command::Query` to the core actor, which
runs:

1. `atlantis_cypher::parse` → AST (calls `tokenize` internally)
2. `atlantis_cypher::plan` → `LogicalOperator` tree
3. `atlantis_cypher::optimize` → the same tree (no rewrite rules yet)
4. `QueryExecutor` walks the tree against `GraphStore` and the HNSW index → rows
5. rows → `ResultSet`

This path has these known gaps, all scheduled for Phase 0:

- `CREATE` skips relationship patterns and returns only the last node created;
  `VarLenExpand` and `CreateRelationship` execute as no-ops.
- Multi-hop `MATCH` binds the far node to every node with that label instead of the
  neighbours. `Expand` follows outgoing edges only, and an untyped relationship pattern
  returns no rows.
- `MATCH ... SET` updates nothing (the planner gives `SetProperty` an empty input).
- `RETURN` or `UNWIND` as the first clause returns no rows.
- `MERGE`, `REMOVE` and `OPTIONAL MATCH` are ignored by the planner.
- The engine never inserts into the HNSW index, so `db.vector.search` returns nothing;
  its label argument is ignored.

See [Query Planner & Executor](15-query-planner-executor.md).

### Actor Model Today

```
 Engine (clonable) ── EngineHandle ── mpsc ──► CoreEngineActor
                                               owns GraphStore + HnswIndex
                                               processes one Command at a time
 placeholder loops (not wired): tiering_actor, checkpoint_actor
```

- The core actor owns all state and processes commands sequentially, so there are no
  locks and no data races. Each call awaits a `oneshot` reply.
- Commands: `Query`, `CreateNode`, `GetNode`, `Checkpoint`, `Shutdown`.
- `Engine::open(path)` restores the graph from a checkpoint at that path if one
  exists. Restored nodes get new IDs while edges keep the old ones. The HNSW index
  always starts empty.
- Checkpoints run on `Engine::checkpoint()` and on shutdown when a disk path is set.
  Eviction and automatic checkpoints never run: the tiering and checkpoint actors are
  placeholder loops, and `checkpoint_interval_secs` is unused. `DELETE` only
  tombstones a node.

Record transactions do not go through the actor. `Engine::transaction` runs its closure
on the calling task against the record store.

See [Engine Runtime](16-engine-runtime.md) for the details.

### How Today's Components Evolve

| Today | Target | Phase |
|---|---|---|
| `CoreEngineActor` (single writer for everything) | Commit sequencer per database plus concurrent snapshot readers | 1 |
| Append-only cold-tier file + bincode checkpoint | `atlantis-kv` with `rocksdb` backend and WAL (`mem` backend done) | 1 |
| Hot tier as the primary store | Cache management over KV | 1 |
| CSR + delta adjacency | In-memory adjacency cache rebuilt from `g/` keys | 1 |
| In-memory HNSW, empty on restart | HNSW persisted in `vx/` keys, cached in memory | 0–1 |
| `NodeId` / `EdgeId` in Cypher | `RecordId` (type and record store done) | 1–2 |
| Two stores (Cypher graph store, record store) | One record store under Cypher | 1–2 |
| Pass-through optimizer | Cost-based optimizer with index selection and join ordering | 2 |
| Embedded `Engine` only | Same `Engine` API over `mem://`, `rocksdb://`, `ws://`, `http://`, `tikv://` | 1, 5–7 |

## Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Query language | Extended Cypher only | One language for graph and non-graph work; familiar to Neo4j users; extensions only where Cypher has nothing. |
| Storage foundation | Ordered transactional KV with MVCC | One mechanism gives transactions, indexes, change feeds and distribution for every model. |
| Default persistent backend | RocksDB | Mature embedded LSM with a WAL and range scans. |
| Distributed backend | TiKV | Raft replication and distributed transactions without building consensus in-house. A native Raft backend is a possible future option, not a commitment. |
| Write concurrency | One commit sequencer per database | Keeps today's simple single-writer reasoning while letting reads scale. |
| Adjacency | CSR + delta kept as a derived cache | Preserves fast traversal without making the CSR the source of truth. |
| Parser | Hand-written recursive descent | Full control over error messages and Cypher quirks. `chumsky` is a declared dependency, but the current parser does not call it. |
| HNSW defaults | M=16, M0=32, ef_construction=200, ef_search=50 | Values recommended by the HNSW paper; tunable. |
| Errors | `thiserror` in libraries | Typed errors with clear messages. |
| Serialization today | serde + bincode | Compact persistence for the graph checkpoint and for record values in KV (with a leading format byte so the format can evolve). Keys use the memcomparable encoding instead. |

## What's Next

- [Data Model](03-data-model.md) — records, RecordIds and the value type system.
- [Storage Engine](04-storage-engine.md) — today's tiers and the KV layer that replaces them.
- [Transactions & Consistency](05-transactions-consistency.md) — MVCC and isolation.
- [Engine Runtime](16-engine-runtime.md) — the actor today and the commit sequencer.
- [Implementation Roadmap](25-implementation-roadmap.md) — the phases referenced above.
