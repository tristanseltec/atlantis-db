# 02 — Architecture

> **Status:** Partially implemented — the seven-crate embedded engine exists: Cypher and the Rust record API on one record store over `atlantis-kv` (`mem` backend with a write-ahead log); the hybrid retrieval path is the current focus; the rest of the layered multi-model architecture is Planned — Phases 1–7
> **Crates:** `atlantis-core`, `atlantis-kv`, `atlantis-storage`, `atlantis-index` (planned), `atlantis-vector`, `atlantis-text` (planned), `atlantis-geo` (planned), `atlantis-cypher`, `atlantis-engine`, `atlantis-auth` (planned), `atlantis-server` (planned), `atlantis-db`, `atlantis-py` (planned), `atlantis-mcp` (planned); commercial, in a separate repository: `atlantis-enterprise` (planned), `atlantis-commerce` (planned)

This chapter describes how Atlantis DB is layered, which crate owns which
responsibility, how a request moves through the system, and how the same engine runs
embedded, as a server and as a cluster. The target design comes first, then the
[hybrid retrieval path](#hybrid-retrieval-path-focus) that the project is built around.
The [commerce layer](#commerce-layer-planned) and the
[edition boundary](#editions-and-extension-points-planned) follow. The "Implemented
Today" section at the end describes the code as it exists now. Why the retrieval path
comes first, and the numbers it must reach, are in
[Positioning & Performance Targets](26-positioning-and-targets.md); the commerce
vertical is in [Commerce](27-commerce.md).

## Target Architecture

Atlantis DB is one engine with one query language, **Atlantis Cypher**, over one record
model (see [Data Model](03-data-model.md)). Every data model (graph, document,
relational, vector, full-text, geospatial, key-value, time series) is stored as keys in
a single transactional key-value layer. Specialised indexes sit between the query
engine and that layer.

```
 Clients: Rust embedded | Python embedded (atlantis-py) | SDKs | CLI | HTTP | WS RPC
         AI agents ── MCP / ACP / UCP
                                │
 atlantis-mcp (planned) ── agent tool gateway     [atlantis-commerce: commerce.* procedures,
 atlantis-server (planned) ── protocols,            connectors, protocol adapters — commercial]
                              sessions, auth handshake
                                │
 atlantis-engine ── session, transactions, permissions enforcement,
                    executor, live-query & event dispatch
                                │
 atlantis-cypher ── lexer → parser → AST → logical plan → cost-based optimizer
                                │
 Index layer: atlantis-vector (filtered HNSW)   atlantis-text (full-text, BM25)
              atlantis-index (B-tree / unique / composite)   atlantis-geo (spatial)
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
| `atlantis-index` (planned) | Background index builds and index statistics. Secondary B-tree, unique and composite indexes themselves are implemented in `atlantis-storage` (`records::schema`) | Phase 2 |
| `atlantis-vector` | HNSW with filtered search, versioned entries for snapshot visibility, SIMD distances, SQ8 quantization, a checkpointed index image | Partial (in-memory HNSW rebuilt from records; the rest is the focus track) |
| `atlantis-text` (planned) | Analyzers, inverted index, BM25 | Phase 3 (focus track) |
| `atlantis-geo` (planned) | Geometry operations, spatial index (geohash / S2-style cells over KV) | Deferred |
| `atlantis-cypher` | Lexer, parser, planner; dialect extensions; cost-based optimizer | Partial (lexer and parser for a Cypher subset plus vector index DDL and `BEGIN`/`COMMIT`; planner Phase 2; other extensions Phases 2–5) |
| `atlantis-engine` | Execution, transactions, permissions, live queries, events, change feeds | Partial (clause interpreter over record transactions, derived vector indexes, periodic checkpoints) |
| `atlantis-auth` (planned) | Users, roles, access methods, JWT, Argon2id password hashing | Phase 5 |
| `atlantis-server` (planned) | HTTP, WebSocket RPC, optional Bolt; the `atlantis` binary | Phase 5 (Bolt: Phase 6) |
| `atlantis-db` | Public facade; one `Engine` API for embedded and remote connections | Partial (embedded Cypher and record transactions on one store; remote Phase 6) |
| `atlantis-py` (planned) | PyO3 bindings over `atlantis-db`: the embedded engine as a Python package | Focus track (ahead of Phase 6) |
| `atlantis-mcp` (planned) | MCP server exposing Cypher-backed tools to agents, with scoped API keys and per-call metering | Commerce C1 (read-only tools) |
| `atlantis-enterprise` (planned, commercial) | Implementations of the extension points: SSO, audit export, PITR backup, replication, larger-than-memory vector index, metering, licence keys | Separate repository; see [Editions](#editions-and-extension-points-planned) |
| `atlantis-commerce` (planned, commercial) | `commerce.*` procedures, connectors, ACP/UCP adapters, merchandising rules, analytics | Separate repository; Commerce C1–C3 |

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
- Commercial crates depend on open crates through the public extension traits only.
  No open crate depends on, or checks for, a commercial crate.

## Deployment Modes

The same engine runs in three shapes. Only the connection URL changes, because every
mode shares the `atlantis-db` `Engine` API and the RPC method surface.

| Mode | What runs | Storage backend | Connection URL | Status |
|---|---|---|---|---|
| **Embedded** | The engine as a library inside the application process | `mem` or `rocksdb` | `mem://`, `rocksdb://path` | Partial (in-memory, or a directory with write-ahead log, today; URLs and `rocksdb` Phase 1) |
| **Server** | One `atlantis start` process serving HTTP, WebSocket RPC and optionally Bolt | `rocksdb` (or `mem`) | `ws://host:7700`, `wss://`, `http://`, `https://` | Phase 5 (Bolt: Phase 6) |
| **Cluster** | Many stateless `atlantis` compute nodes over a TiKV cluster | `tikv` | `tikv://pd1:2379,pd2:2379` (node side); clients use `ws://` / `http://` | Phase 7 (deferred) |

Embedded is the mode the project competes in. The embedded engine is reachable from Rust
and, through `atlantis-py`, from Python in the same process; the server and cluster
reuse it unchanged.

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
store-wide lock, and made durable outside it with group commit. The per-database
commit sequencer below is still planned.

- **Writes**: each database has one **commit sequencer**, which replaces today's
  store-wide commit lock. It orders commits and validates conflicts.
- **Reads**: run concurrently on MVCC snapshots and never wait for the sequencer.
- **Background workers**: index builds, adjacency-cache maintenance, change-feed
  retention and time-series downsampling run outside the request path.
- **Isolation**: snapshot isolation by default, serializable on request. See
  [Transactions & Consistency](05-transactions-consistency.md).

## Hybrid Retrieval Path (Focus)

> **Status:** Target design. Today `db.vector.search` exists without filtering, BM25 or
> read-your-writes; everything else in this section is planned. Targets and the
> benchmark are in [Positioning & Performance Targets](26-positioning-and-targets.md).

The query Atlantis DB is built to win is a **filtered vector search, fused with BM25,
followed by a graph expansion, on one MVCC snapshot, with the transaction's own writes
visible**. This section describes the components on that path and how they keep
search consistent with the record store.

### Query Shape

```cypher
CALL db.search.hybrid({text: 'memory_text', vector: 'memory_vec'}, $q, $qvec, 10)
  YIELD node, score
  WHERE node.user = $user
MATCH (node)-[:ABOUT]->(e:Entity)-[:RELATED*1..2]->(ctx)
RETURN node.text, score, collect(DISTINCT ctx.name) AS context
```

The planner pushes the `WHERE` that follows `YIELD` into the search operators, so the
filter shapes the search instead of discarding hits after it. Today that `WHERE` runs
after the search and can return fewer than `k` rows; with pushdown it returns `k` rows
whenever `k` matching records exist.

### Physical Plan

```
 Project / Aggregate
   └─ Expand(RELATED*1..2) ← Expand(ABOUT)              adjacency prefix scans (cache later)
        └─ HybridFuse(RRF, k=10)
             ├─ VectorTopK(memory_vec, $qvec, allow)     filtered HNSW + tx overlay
             └─ TextTopK(memory_text, $q, allow)         BM25 over postings + tx overlay
                  allow = AllowSet(node.user = $user)    bitmap from label/property index
```

All operators read through the same `RecordTx`, so they share one snapshot
(`Versionstamp`) and see the transaction's uncommitted writes.

### Components

| Component | Crate | Design |
|---|---|---|
| **Allow set** | `atlantis-engine`, `atlantis-index` | The pushed-down predicate is evaluated once into a compressed bitmap over the index's internal slot ids, from a property index when one exists, else by a scan of the label. Its cardinality picks the search strategy. |
| **Filtered HNSW** | `atlantis-vector` | Three strategies chosen by selectivity: **exact** (brute-force distances over the allow set when it is small, a few thousand slots), **filtered traversal** (walk the graph through all nodes but admit only allowed ones to the result heap; when a neighbourhood is mostly filtered out, also expand two-hop neighbours to keep the search connected), and **post-filter** (broad predicates: over-fetch and filter). Recall must stay ≥ 0.95 at 1% selectivity. |
| **Vector storage** | `atlantis-vector` | Vectors live in one contiguous `f32` arena indexed by slot, not in per-node allocations (*done*). Distance kernels use SIMD with runtime dispatch (AVX2 *done*; AVX-512, NEON open). Optional SQ8 codes for traversal, with an exact `f32` re-rank of the top candidates. |
| **Snapshot visibility** | `atlantis-vector`, `atlantis-text` | Each index entry carries the `Versionstamp` that inserted it and, once deleted or replaced, the one that removed it. A search at snapshot `s` admits only entries visible at `s`. Removed entries are physically dropped when they fall behind the KV pruning horizon, the same rule `MemStore` uses for record versions. |
| **Transaction overlay** | `atlantis-engine` | A transaction keeps the vectors and text it wrote but has not committed in a small per-transaction overlay. Search runs exactly over the overlay and merges it with the index hits, after masking committed entries that the transaction overwrote or deleted. This is what makes a memory readable in the transaction that wrote it. |
| **Commit apply** | `atlantis-engine` | After a commit, its changed records are applied to the shared indexes, tagged with the commit's `Versionstamp`. Because entries are versioned, a reader at an older snapshot is unaffected by entries applied after it began. |
| **BM25 index** | `atlantis-text` | An in-memory inverted index (postings with term frequencies and document lengths), versioned like the vector index, derived from records and checkpointed the same way. |
| **Fusion** | `atlantis-engine` | `HybridFuse` merges the ranked lists by reciprocal rank fusion (`1 / (60 + rank)` by default, weights per input), then re-applies the allow set as a safety check. |
| **Index image** | `atlantis-vector`, `atlantis-text` | At each checkpoint, every built index writes an image file tagged with the checkpoint's `Versionstamp`. On open, the engine loads the image and replays only records changed after it, so a 1M-vector database opens in under a second. A missing or mismatched image falls back to a rebuild from records, so images stay derived data. |
| **Concurrency** | `atlantis-engine` | One lock per index (*done*). Searches take a read lock; commit apply takes a short write lock per batch. Search and index builds run on a query worker pool, not on the async executor (open). |
| **Group commit** | `atlantis-kv` | *Done* (in `MemStore`, ahead of the sequencer). The commit path batches the write-ahead-log fsyncs of concurrent commits, so durable ingest scales with writers instead of being bound by one fsync per commit. |

### Why Versioned Index Entries

Today the vector index reflects the latest committed state and is updated after the
commit returns, so a reader can see a hit whose record its snapshot cannot read, or
miss one it can. Versioned entries plus the transaction overlay make search obey the
same isolation as `MATCH`. Read-your-writes and snapshot-consistent search are the
feature, not an implementation detail, so they are designed in rather than added later.

### Python Embedding

`atlantis-py` exposes `Engine`, `Transaction` and `execute` through PyO3. Vectors cross
the boundary as NumPy buffers without copying where the layout allows. Calls release
the GIL while the engine works. This puts the embedded engine where most retrieval and
agent code is written, ahead of the server and remote SDKs.

## Commerce Layer (Planned)

> **Status:** Target design, Stages C0–C4 in [27 — Commerce](27-commerce.md). Nothing
> here is implemented.

Commerce is the first vertical built on the hybrid retrieval path. The engine gains
general-purpose features that commerce needs; the commerce product sits above the
engine and talks to it only through Cypher and the public API.

```
 Shoppers (web, app)      Agents (assistants, procurement)     Platform of record
        │                     │ MCP / ACP / UCP                (Shopify, commercetools,
        │                     ▼                                  SAP, ERP)
        │              atlantis-mcp ── tools: search, compare,         │
        │              availability, price, quote, reserve             │ ingest connectors
        │                     │                                        │ (change feeds in)
        ▼                     ▼                                        ▼
 atlantis-commerce ── commerce.search · commerce.price · commerce.reserve · quotes
                      merchandising rules · analytics · protocol adapters
                                      │  Cypher, one transaction per tool call
 atlantis-engine ── filtered hybrid search · facets · escrow reservations · Decimal
                                      │
                         record store (one MVCC snapshot)  ──► change feed out
```

### Engine Features Commerce Drives (open core)

| Feature | Design |
|---|---|
| **`Decimal`** | Exact fixed-point `Value` variant with arithmetic and comparison in Cypher; prices, totals and taxes never touch floats. |
| **Bitmap facets** | Facet fields keep per-value bitmaps over the same slot space as the allow set. A facet count is the cardinality of `facetBitmap ∧ resultBitmap`, so counts for a filtered result cost microseconds instead of a scan. Bitmaps are versioned like vector entries. |
| **Escrow reservations** | A stock counter is updated by commutative `reserve(n)` / `release(n)` operations that check a bound (`qty - reserved ≥ 0`) at commit instead of conflicting on the row. Concurrent checkouts on a hot SKU commit together until the bound would break, so there is no oversell and no retry storm. |
| **Multi-vector fusion** | Several vector indexes per label (text, image), fused with BM25 by the same `HybridFuse` operator. |
| **Unique constraints** | A minimal unique index on SKU, MPN and GTIN, ahead of full Phase 2 schema. |
| **Change feeds** | Ingest connectors apply platform changes as transactions; an outbound feed publishes orders and reservations. A minimal slice of Phase 4. |

### Entitlements

An account's entitlement is evaluated into the allow set before search, from its
contracts' `ENTITLES` edges and the account hierarchy. The allow set is cached per
contract version, so a typical call pays only the bitmap intersection. Prices come from
`commerce.price` reading `PRICES` tiers on the same snapshot. Leak tests are
property-based: no SKU or price outside the contract may ever be returned.

### Agent Tool Calls

Each `atlantis-mcp` tool runs as one Cypher transaction. Read tools use a snapshot;
`quote` and `reserve` are write transactions, so what the agent was told and what it
reserved come from the same state. Each call is metered and, with the commerce
product, written to an agent audit trail (tool, inputs, snapshot `Versionstamp`,
result).

## Editions and Extension Points (Planned)

> **Status:** Target design.

The open core is every crate in this repository, under Apache-2.0. It includes the
whole hybrid retrieval path and every engine feature in the commerce layer. Paid
features live in a separate repository and plug in through traits the open crates
define, so the core never contains disabled code, licence checks or telemetry it does
not need.

| Extension point (open trait) | Community implementation | Commercial implementation |
|---|---|---|
| `AuthProvider` | Users and roles in the system database | OIDC / SAML SSO, SCIM provisioning |
| `AuditSink` | None | Audit log with export (file, SIEM) |
| `BackupTarget` | Manual full backup to a directory | Online incremental backup, point-in-time recovery, object storage |
| `Replicator` | None (single node) | Log-shipping replicas, failover |
| `VectorIndexProvider` | In-memory HNSW (filtered, versioned) | Larger-than-memory index (DiskANN-style), tiered storage |
| `KeyProvider` | Unencrypted, or a local key file | Customer-managed keys (KMS) |
| `Meter` | No-op | Per-tenant usage for OEM and cloud billing |
| `ProcedureRegistry` | Built-in procedures | `commerce.*` procedures from `atlantis-commerce` |

Rules:

- A commercial crate registers implementations at `Engine` build time. With none
  registered, the engine behaves exactly as Community.
- The licence key is checked only inside commercial crates.
- Data written by any edition opens in Community. Upgrading or downgrading never needs a
  migration; only the features change.

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
   HNSW indexes already work this way: only their definitions are stored. Index image
   files speed up opening but are never the source of truth.
6. **Honest layering.** The parser and planner do not read data, storage does not know
   Cypher, and the server does not execute queries.
7. **Retrieval first.** The hybrid retrieval path is the product. Work that makes it
   faster, more accurate or more consistent comes before new data models, and a change
   that slows it needs a benchmark-backed justification.
8. **Search obeys isolation.** Every index answers at the reader's snapshot and includes
   the transaction's own writes, exactly as `MATCH` does.
9. **Open core, clean boundary.** Engine capabilities are open; paid features are
   alternative implementations of open traits, never switches inside open code. Data
   is portable across editions.

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
| Store-wide staging lock in `MemStore`, group commit outside it | Commit sequencer per database plus concurrent snapshot readers | 1 |
| `mem` backend with write-ahead log and checkpoint file | Adds the `rocksdb` backend (default persistent) | 1 |
| Traversal by prefix scans of adjacency keys | Plus an in-memory adjacency cache rebuilt from `g/` keys | 1 (open) |
| HNSW rebuilt in memory from records on first search | Checkpointed index image plus replay of later commits; `vx/` keys for the distributed backend | Focus track |
| Per-index read-write locks; search sees the latest commit | Versioned entries and a transaction overlay for snapshot-consistent, read-your-writes search | Focus track |
| Field comparisons after `YIELD` pushed into a filtered HNSW search | Also labels, nested fields and `OR`; pushed into BM25 search too | Focus track |
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
| Product focus | Transactional hybrid retrieval, embedded, before breadth | The open position among competitors; see [Positioning](26-positioning-and-targets.md). Time-series, spatial indexes, the KV API, Bolt and the cluster are deferred. |
| Search consistency | Versioned index entries plus a per-transaction overlay | Read-your-writes and snapshot-consistent search without writing every HNSW edge through the KV commit path. |
| Index persistence on local backends | Checkpointed image file, then replay | Keeps the commit path cheap (no neighbour-list writes per insert) and opens fast; `vx/` keys stay the design for TiKV, where compute nodes share no disk. |
| Filtered vector search | Strategy chosen per query from allow-set cardinality | No single strategy keeps recall and latency at both 0.1% and 50% selectivity. |
| Python | Embedded PyO3 package before remote SDKs | Retrieval and agent code is mostly Python; an in-process package needs no server. |
| First vertical | Commerce, B2B first | Entitlement filters at ≤ 1% selectivity, part-number + semantic + compatibility search and transactional quotes exercise exactly the focus feature. See [Commerce](27-commerce.md). |
| Hot stock counters | Escrow reservations checked at commit | Optimistic MVCC alone turns a popular SKU into a retry storm; bounded commutative updates keep zero oversell without serializing checkouts. |
| Facets | Versioned bitmaps sharing the allow set's slot space | Facet counts on filtered results become bitmap intersections, not scans. |
| Edition boundary | Open traits, commercial implementations in a separate repository | Keeps the Apache-2.0 core trustworthy and free of licence logic. |
| Errors | `thiserror` in libraries | Typed errors with clear messages. |
| Serialization today | serde + bincode | Compact persistence for record values in KV (with a leading format byte so the format can evolve), log records and the checkpoint file. Keys use the memcomparable encoding instead. |

## What's Next

- [Data Model](03-data-model.md) — records, RecordIds and the value type system.
- [Storage Engine](04-storage-engine.md) — the KV layer, write-ahead log and record layout.
- [Transactions & Consistency](05-transactions-consistency.md) — MVCC and isolation.
- [Engine Runtime](16-engine-runtime.md) — the datastore today and the commit sequencer.
- [Implementation Roadmap](25-implementation-roadmap.md) — the phases referenced above.
- [Positioning & Performance Targets](26-positioning-and-targets.md) — competitors,
  the benchmark and the numbers the hybrid retrieval path must reach.
- [Commerce](27-commerce.md) — the first vertical: model, targets and stages.
