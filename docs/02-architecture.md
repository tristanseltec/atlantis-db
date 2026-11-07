# 02 — Architecture

> **Status:** Partially implemented — the seven-crate embedded engine exists: Cypher and the Rust record API on one record store over `atlantis-kv` (`mem` backend with a write-ahead log); the rest of the layered multi-model architecture is Planned — Phases 1–7
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
 atlantis-storage ── record store, edge store, adjacency and record caches
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
| Storage | Record and edge stores, adjacency and record caches | [Storage Engine](04-storage-engine.md) |
| KV | Ordered transactional key space, MVCC, pluggable backends | [Storage Engine](04-storage-engine.md), [Transactions & Consistency](05-transactions-consistency.md) |
| Core | Shared types, errors, configuration | [Data Model](03-data-model.md), [Configuration](22-configuration.md) |

### Crate Map

| Crate | Role | Status |
|---|---|---|
| `atlantis-core` | Types: `Value`, `RecordId`, `Geometry`, `Datetime`, `Duration`, errors, config | Partial (record types implemented; `Decimal`, `Range` Phase 2) |
| `atlantis-kv` | `KvStore` / `KvTransaction` traits, key encoding, MVCC; features `kv-mem`, `kv-rocksdb`, `kv-tikv` | Partial (traits, encoding, MVCC and the `mem` backend with a write-ahead log implemented; `rocksdb` and backend features Phase 1, `tikv` Phase 7) |
| `atlantis-storage` | Record and edge stores on top of `atlantis-kv`; optional adjacency and record caches | Partial (record store implemented and used by Cypher; caches later) |
| `atlantis-index` (planned) | Secondary B-tree, unique and composite indexes; index manager and background index builds | Phase 2 |
| `atlantis-vector` | HNSW, persisted through `atlantis-kv` | Partial (in-memory HNSW rebuilt from records; persistence in KV later) |
| `atlantis-text` (planned) | Analyzers, inverted index, BM25 | Phase 3 |
| `atlantis-geo` (planned) | Geometry operations, spatial index (geohash / S2-style cells over KV) | Phase 3 |
| `atlantis-cypher` | Lexer, parser, planner; dialect extensions; cost-based optimizer | Partial (lexer and parser for a Cypher subset plus vector index DDL and `BEGIN`/`COMMIT`; planner Phase 2; other extensions Phases 2–5) |
| `atlantis-engine` | Execution, transactions, permissions, live queries, events, change feeds | Partial (clause interpreter over record transactions, derived vector indexes, periodic checkpoints) |
| `atlantis-auth` (planned) | Users, roles, access methods, JWT, Argon2id password hashing | Phase 5 |
| `atlantis-server` (planned) | HTTP, WebSocket RPC, optional Bolt; the `atlantis` binary | Phase 5 (Bolt: Phase 6) |
| `atlantis-db` | Public facade; one `Engine` API for embedded and remote connections | Partial (embedded Cypher and record transactions on one store; remote Phase 6) |

### Dependency Rules

- Dependencies point down the layer diagram. A crate never depends on a crate above it.
- `atlantis-core` has no internal dependencies.
- Index crates and `atlantis-storage` reach the disk only through `atlantis-kv`, so a
  new backend (for example TiKV) needs no changes above the KV layer.
- `atlantis-cypher` depends only on `atlantis-core`. It produces syntax trees (and,
  from Phase 2, plans); it never reads data. `atlantis-engine` connects them to
  storage and indexes.
- `atlantis-server` and `atlantis-auth` sit above the engine. The embedded build does
  not compile them.
- Users depend on `atlantis-db` only.

## Deployment Modes

The same engine runs in three shapes. Only the connection URL changes, because every
mode shares the `atlantis-db` `Engine` API and the RPC method surface.

| Mode | What runs | Storage backend | Connection URL | Status |
|---|---|---|---|---|
| **Embedded** | The engine as a library inside the application process | `mem` or `rocksdb` | `mem://`, `rocksdb://path` | Partial (in-memory, or a directory with write-ahead log, today; URLs and `rocksdb` Phase 1) |
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
 3. atlantis-engine   open a transaction: snapshot for reads               │  done
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

Today Cypher statements and record transactions run as optimistic MVCC transactions
on the `mem` backend; commits are validated and logged one at a time under a
store-wide lock. The per-database commit sequencer below is still planned.

- **Writes**: each database has one **commit sequencer**, which replaces today's
  store-wide commit lock. It orders commits and validates conflicts.
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
5. **Caches are derived.** Adjacency caches, HNSW graphs and full-text structures in
   memory can always be rebuilt from KV, which keeps compute nodes stateless. Today's
   HNSW indexes already work this way: only their definitions are stored.
6. **Honest layering.** The parser and planner do not read data, storage does not know
   Cypher, and the server does not execute queries.

## Implemented Today

The current code is an embedded engine in seven crates with **one store**. Cypher
(`Engine::execute`, `Transaction::execute`) and the Rust record API
(`Engine::transaction`, `Engine::read`) read and write the same records, kept in
`atlantis-kv` on the in-memory `mem` backend. An engine opened on a directory makes
every commit durable through a write-ahead log. There is no server, authentication or
schema yet.

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
atlantis-engine        ← core, kv, storage, vector, cypher
    ↑
atlantis-db            ← core, engine, kv, storage (public facade)
```

| Crate | Responsibility today |
|---|---|
| `atlantis-core` | `Value` (with the extended types), record types (`DatabaseRef`, `RecordId`, `Record`, `Edge`), `Direction`, `NodeId` (the HNSW entry id), config, `AtlantisError` |
| `atlantis-kv` | `KvStore` / `KvTransaction` traits, memcomparable key encoding and layout, `MemStore` MVCC backend, write-ahead log and checkpoint file |
| `atlantis-storage` | `RecordStore` / `RecordTx`: records, edges with adjacency entries, secondary labels, the table and vector-index catalog |
| `atlantis-vector` | In-memory HNSW index and distance metrics |
| `atlantis-cypher` | Hand-written lexer and recursive-descent parser producing an AST (`parse`, `parse_script`); there is no planner |
| `atlantis-engine` | `Datastore`: opens the store, runs Cypher scripts clause by clause against record transactions, keeps the derived vector indexes, runs periodic checkpoints |
| `atlantis-db` | `Engine` (Cypher and record transactions), `Transaction`, `Config`, `Params`, `ResultSet`, `Row`, re-exported value, record and transaction types |

### Query Flow Today

`Engine::execute(script, params)` calls `Datastore::execute`, which:

1. parses the script with `atlantis_cypher::parse_script` into statements;
2. runs each statement in its own record transaction, or groups statements between
   `BEGIN` and `COMMIT` into one transaction;
3. interprets each clause in order: every clause maps a list of rows (variable →
   value) to a new list, reading and writing records through the transaction;
4. commits, then updates any built vector index from the record ids the commit
   changed;
5. returns the result of the last statement as a `ResultSet`.

There is no logical plan and no optimizer: `MATCH` scans the label index of the first
label in each node pattern (or looks a record up directly when the pattern pins `id`)
and expands along adjacency entries. See
[Query Planner & Executor](15-query-planner-executor.md).

### Runtime Today

- There is no actor and no command channel. Calls run on the calling task.
  Transactions are optimistic MVCC transactions; commits are validated one at a time
  under a store-wide lock.
- A directory-backed engine takes a `LOCK` file, so one process opens a directory at a
  time. A background thread checkpoints every `checkpoint_interval_secs`.
- Opening a directory that holds only a legacy `checkpoint.db` (from the removed
  graph store) fails with an error that says to move it aside.

See [Engine Runtime](16-engine-runtime.md) for the details.

### How Today's Components Evolve

| Today | Target | Phase |
|---|---|---|
| Store-wide commit lock in `MemStore` | Commit sequencer per database plus concurrent snapshot readers | 1 |
| `mem` backend with write-ahead log and checkpoint file | Adds the `rocksdb` backend (default persistent) | 1 |
| Traversal by prefix scans of adjacency keys | Plus an in-memory adjacency cache rebuilt from `g/` keys | 1 (open) |
| HNSW rebuilt in memory from records on first search | HNSW persisted in `vx/` keys, cached in memory | 1 (open) |
| Clause-by-clause interpreter | Planner and cost-based optimizer with index selection and join ordering | 2 |
| Embedded `Engine` only | Same `Engine` API over `mem://`, `rocksdb://`, `ws://`, `http://`, `tikv://` | 1, 5–7 |

## Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Query language | Extended Cypher only | One language for graph and non-graph work; familiar to Neo4j users; extensions only where Cypher has nothing. |
| Storage foundation | Ordered transactional KV with MVCC | One mechanism gives transactions, indexes, change feeds and distribution for every model. |
| Default persistent backend | RocksDB | Mature embedded LSM with a WAL and range scans. |
| Distributed backend | TiKV | Raft replication and distributed transactions without building consensus in-house. A native Raft backend is a possible future option, not a commitment. |
| Write concurrency | One commit sequencer per database | Keeps commit ordering simple while letting reads scale. |
| Adjacency | Adjacency entries in KV on both endpoints are the source of truth; the planned in-memory adjacency cache is derived | Traversal is a prefix scan today, and a cache can speed it up without becoming the source of truth. |
| Parser | Hand-written recursive descent | Full control over error messages and Cypher quirks. |
| HNSW defaults | M=16, M0=32, ef_construction=200, ef_search=50 | Values recommended by the HNSW paper; tunable. |
| Errors | `thiserror` in libraries | Typed errors with clear messages. |
| Serialization today | serde + bincode | Compact persistence for record values in KV (with a leading format byte so the format can evolve), log records and the checkpoint file. Keys use the memcomparable encoding instead. |

## What's Next

- [Data Model](03-data-model.md) — records, RecordIds and the value type system.
- [Storage Engine](04-storage-engine.md) — the KV layer, write-ahead log and record layout.
- [Transactions & Consistency](05-transactions-consistency.md) — MVCC and isolation.
- [Engine Runtime](16-engine-runtime.md) — the datastore today and the commit sequencer.
- [Implementation Roadmap](25-implementation-roadmap.md) — the phases referenced above.
