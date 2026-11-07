# 04 — Storage Engine

> **Status:** Partially implemented — the hot/cold graph store (used by Cypher) and `atlantis-kv` with the `mem` backend, key encoding and record store are implemented; `rocksdb` and sync modes Planned — Phase 1, secondary indexes Phase 2, `tikv` Phase 7
> **Crates:** `atlantis-storage`, `atlantis-kv`, `atlantis-core`

This chapter covers how Atlantis DB stores data. Two stores exist side by side during the migration:

- **Graph store (implemented today)** — the store that Cypher (`Engine::execute`) uses: an in-memory hot tier with CSR + delta adjacency, an append-only cold tier, LRU tiering, and whole-store checkpoints.
- **Transactional KV layer and record store** — the target foundation: the `atlantis-kv` ordered key-value layer, the memcomparable key encoding, the `mem` / `rocksdb` / `tikv` backends, the record and edge store, and the role CSR keeps as an adjacency cache. `atlantis-kv` with the `mem` backend and the record store are implemented; the rest is planned.

The two stores do not share data yet. Cypher queries read and write only the graph store, and record transactions ([21 — API Reference](21-api-reference.md)) read and write only the record store.

Transactions, isolation, and durability guarantees are covered in [05 — Transactions & Consistency](05-transactions-consistency.md).

---

## Graph store (implemented today)

### Overview

`GraphStore` (`crates/atlantis-storage/src/store.rs`) is a facade over a hot tier and an optional cold tier. All live data is held in the hot tier. The cold tier holds nodes that were evicted under memory pressure. Durability comes from explicit checkpoints of the hot tier.

```
┌──────────────────────────────────────────────┐
│              GraphStore (facade)             │
│                                              │
│  ┌───────────────┐     ┌──────────────────┐  │
│  │   Hot Tier    │     │    Cold Tier     │  │
│  │  (in memory)  │     │    (on disk)     │  │
│  │  Nodes[Vec]   │     │  cold_tier.db    │  │
│  │  Edges[Vec]   │     │  (append-only)   │  │
│  │  CSR + delta  │     │  page index:     │  │
│  │  Label index  │     │  NodeId→Location │  │
│  │  LRU tracker  │     │                  │  │
│  └───────┬───────┘     └────────┬─────────┘  │
│          └──── TieringManager ──┘            │
│                     │                        │
│        checkpoint.db (bincode snapshot)      │
└──────────────────────────────────────────────┘
```

### Hot tier

`HotTier` (`crates/atlantis-storage/src/hot_tier.rs`) keeps nodes in `Vec<Option<Node>>` and edges in `Vec<Option<Relationship>>`, indexed by the numeric `NodeId` / `EdgeId`. Access by ID is O(1).

IDs are allocated sequentially from `next_node_id` / `next_edge_id` and are never reused:

- **Deleting a node** adds its ID to a tombstone set and removes it from the label index. The node's slot and its edges stay in memory; reads and neighbor lookups filter tombstoned IDs out.
- **Deleting an edge** clears its slot and removes it from the delta log. Edges already compacted into the CSR are dropped at the next compaction only if one of their endpoints is tombstoned.
- The struct has free-slot lists, but nothing populates them yet.

A **label index** (`HashMap<Label, Vec<NodeId>>`) backs label scans. It is updated on insert and delete.

### CSR + delta adjacency

Adjacency is kept per relationship type, **outgoing direction only**:

- **CSR** (`CsrAdjacency` in `csr.rs`): contiguous `offsets` / `targets` / `edge_ids` arrays. `offsets[n]..offsets[n+1]` is the slice of outgoing edges of node `n`, so out-degree and neighbor iteration are cache-friendly. `CsrStore` holds one `CsrAdjacency` per `RelType`.
- **Delta log** (`DeltaLog` in `delta.rs`): a `HashMap<NodeId, Vec<(NodeId, EdgeId)>>` of edges inserted since the last compaction.

`get_neighbors(from, rel_type)` merges the delta entries and the CSR slice for that type and filters tombstoned targets. There is no incoming-edge index, so reverse traversal is not supported by the store today.

### Compaction

Compaction rebuilds the CSR of one relationship type:

1. Collect existing CSR edges whose endpoints are not tombstoned.
2. Drain the delta log, applying the same filter.
3. Rebuild the CSR from the combined edge list.

`GraphStore::insert_edge` compacts a type automatically when its delta reaches `compaction_threshold` (default 1000, see [22 — Configuration](22-configuration.md)). `GraphStore::compact_all()` compacts every type that has delta entries.

### LRU tracker and memory estimate

`LruTracker` (`lru.rs`) records node accesses in a `VecDeque` with a position map. Inserts and `GraphStore::get_node` touch it. `HotTier::memory_usage()` returns a rough byte estimate from node and edge counts, label counts, and property counts. It does not account for adjacency structures.

### Cold tier

`ColdTier` (`cold_tier.rs`) is a single append-only file, `cold_tier.db`, in the database directory:

- **Write** serializes a node with bincode, appends it, and records `PageLocation { offset, size }` in an in-memory page index.
- **Read** seeks to the recorded location and deserializes.
- **Remove** drops the page-index entry. The bytes stay in the file; there is no space reclamation.

### Tiering manager

`TieringManager` (`tiering.rs`) moves nodes between tiers:

- `maybe_evict()` does nothing when `memory_budget_bytes` is unset or not exceeded. Otherwise it takes up to 10 least-recently-used nodes, removes each from the hot tier, and writes it to the cold tier.
- `promote(id)` reads a node from the cold tier and removes its page-index entry, but does not re-insert it into the hot tier.

Current behaviour to be aware of:

- No engine code path calls `GraphStore::maybe_evict()`, so eviction does not happen at runtime (see [16 — Engine Runtime](16-engine-runtime.md)).
- An evicted node is invisible to queries: the executor reads through `get_node_hot`, and `GraphStore::get_node` does not complete promotion.

Both are Phase 0 items in [25 — Implementation Roadmap](25-implementation-roadmap.md) ("stable-ID promotion").

### GraphStore facade

`GraphStore` is the only storage API the Cypher engine (`atlantis-engine`) uses. Its main operations are listed below; the source file is authoritative.

| Method | Purpose |
|---|---|
| `new_in_memory()` / `with_config(config)` / `open(path)` | Create an in-memory store, a configured store, or open a directory (restoring `checkpoint.db` if present) |
| `insert_node`, `insert_edge` | Create a node or a directed edge; returns the new ID |
| `get_node_hot`, `get_node_owned`, `get_edge` | Read from the hot tier |
| `get_neighbors`, `get_nodes_by_label`, `get_all_nodes` | Traversal and scans used by the executor |
| `set_property`, `remove_property` | Mutate node properties |
| `delete_node`, `delete_edge` | Tombstone a node, clear an edge |
| `checkpoint`, `maybe_evict`, `compact_all` | Persistence and maintenance |

### Persistence

`persistence.rs` writes the whole hot tier to `checkpoint.db` as one bincode-encoded `SerializedStore` with magic bytes `b"ATLANTIS"` and format version `1`. It contains live nodes, edges, a label index, ID counters, and the cold-tier page index.

- **Checkpoint** runs on `engine.checkpoint()` and on shutdown. The file is overwritten in place. Nothing is persisted between checkpoints, so a crash loses all writes since the last checkpoint.
- **Restore** validates the magic bytes and version, then re-inserts nodes and edges through the normal insert path. Because IDs are allocated sequentially, a store with deleted nodes gets **new node IDs** on restore, while edges keep their stored endpoint IDs. Restore also uses the default compaction threshold instead of the configured one.
- The HNSW vector index is not part of the checkpoint (see [10 — Vector Index](10-vector-index.md)).

### Data flow example

```
CREATE (a:Person {name: 'Alice'})
  └─ GraphStore::insert_node(["Person"], {name: "Alice"})  → NodeId(0)
       HotTier: allocate id, store in nodes[0], update label index, touch LRU

GraphStore::insert_edge(NodeId(0), NodeId(1), "KNOWS", {}) → EdgeId(0)
       HotTier: store in edges[0], append to delta["KNOWS"][0], touch LRU
       if delta["KNOWS"].len() >= compaction_threshold → compact("KNOWS")

GraphStore::get_neighbors(NodeId(0), "KNOWS")
       delta["KNOWS"][0] ++ CSR["KNOWS"].slice(0), minus tombstones
```

Relationship creation is available through the storage API but not yet through Cypher `CREATE` (see [15 — Query Planner & Executor](15-query-planner-executor.md)).

---

## Transactional KV layer and record store

The target storage stack separates **what** is stored (records, edges, indexes) from **where** bytes live (a pluggable transactional KV backend):

```
 atlantis-storage ── record store, edge store, CSR adjacency cache, cache management
          │
 atlantis-kv ─────── KvStore / KvTransaction traits, key encoding, MVCC
          │
   backends: mem | rocksdb (default persistent) | tikv (distributed, Phase 7)
```

Every model (graph, document, relational, key-value, time-series, vector, full-text, geospatial) is stored as ordered keys in one keyspace. Indexes, change feeds, and the catalog live in the same keyspace and are written in the same transaction as the data they describe.

Implemented today: `atlantis-kv` with the `mem` backend, the record, catalog, adjacency and label keys, and the record store. Planned: the `rocksdb` and `tikv` backends, the CSR adjacency cache, and the `ix/`, `ft/`, `vx/`, `cf/` and `kv/` keys.

### `atlantis-kv` (Implemented — `mem` backend only)

`atlantis-kv` ([`crates/atlantis-kv/src/kv.rs`](../crates/atlantis-kv/src/kv.rs)) exposes an ordered, transactional key-value API with snapshot reads. The traits are **synchronous**: an earlier draft of this design used `async` methods, and that is no longer the plan.

| Operation | Meaning |
|---|---|
| `KvStore::begin(mode, isolation)` | Start a transaction on a snapshot of the latest commit. `mode` is `TxMode::ReadOnly` or `TxMode::ReadWrite`; `isolation` is `Isolation::Snapshot` (default) or `Isolation::Serializable`. |
| `get(key)` | Read one key as of the snapshot, including the transaction's own writes |
| `set(key, value)` / `delete(key)` | Buffer a write in the transaction |
| `scan(range, limit)` | Return pairs in a half-open `KeyRange`, in byte order |
| `commit()` | Validate and atomically apply all writes; returns a `Versionstamp` |
| `cancel()` | Discard the transaction. Dropping an unfinished transaction also cancels it. |

The trait definitions in `kv.rs` are authoritative. Isolation and conflict rules are in [05 — Transactions & Consistency](05-transactions-consistency.md).

There are no Cargo backend features yet. `kv-mem`, `kv-rocksdb`, and `kv-tikv` features and selection by connection URL are planned with the backends that need them.

### Key encoding (Implemented)

Keys are **memcomparable**: comparing encoded keys byte-by-byte gives the same order as comparing the logical values. A range scan over a key prefix therefore returns records, adjacency entries, or index entries in logical order, on every backend. The layout is defined in [`crates/atlantis-kv/src/keys.rs`](../crates/atlantis-kv/src/keys.rs) and the encoding in [`encoding.rs`](../crates/atlantis-kv/src/encoding.rs).

In the table, `/ns`, `/r` and similar are literal markers, and `{…}` is an encoded component. Names are escaped strings, so a marker can never be confused with data.

| Prefix | Holds | Status |
|---|---|---|
| `/sys/ns/{ns}` | Namespace definition | Implemented |
| `/sys/ns/{ns}/db/{db}` | Database definition | Implemented |
| `/sys/ns/{ns}/db/{db}/tb/{tb}` | Table definition (`TableDef`) | Implemented |
| `/ns/{ns}/db/{db}/tb/{tb}/r/{key}` | A record (node, edge, document, row, time-series point) | Implemented |
| `.../tb/{tb}/g/{nodeKey}/{dir}/{edgeTable}{edgeKey}` | An adjacency entry under the node's table; `dir` is `o` (outgoing) or `i` (incoming) | Implemented |
| `/ns/{ns}/db/{db}/lb/{label}/{table}{key}` | A secondary label index entry | Implemented |
| `.../tb/{tb}/ix/{index}/{encodedValues}/{key}` | A secondary index entry | Phase 2 |
| `.../tb/{tb}/ft/{index}/...` | Full-text postings and statistics | Phase 3 |
| `.../tb/{tb}/vx/{index}/...` | HNSW graph layers and vectors | Phase 3 |
| `.../cf/{tb}/{versionstamp}` | A change-feed entry | Phase 4 |
| `/ns/{ns}/db/{db}/kv/d/{key}` | A raw key-value entry (see [13 — Key-Value & Time-Series](13-key-value-time-series.md)) | Phase 3 |
| `/ns/{ns}/db/{db}/kv/x/{expiresAt}/{key}` | TTL expiry queue entry, scanned by the expiry worker | Phase 3 |
| `/sys/...` (other) | Fields, indexes, users, cluster membership | Phases 2, 5, 7 |

Encoding rules for key components:

- **Strings** and bytes escape every `0x00` byte and end with a terminator, so `"ab"` sorts before `"abc"` and a component never bleeds into the next one.
- **Record keys** are tagged by kind, so keys of different kinds sort integer < string < UUID < ULID < array, then by value.
- **Integers** in record keys are written big-endian with the sign bit flipped, so negative keys sort before positive ones.
- **Numbers inside values** (array key elements) share one tag and sort numerically across `Integer` and `Float`. An integer is prefixed with the largest `f64` not above it, so the order stays numeric even above 2^53. `Integer(1)` and `Float(1.0)` still encode to different bytes (see [03 — Data Model](03-data-model.md)).
- **Floats** use the standard order-preserving bit transform.
- **UUID** and **ULID** keys are written as their 16 raw bytes. ULIDs are time-ordered, so the default auto-generated key gives insertion-ordered scans.
- **Arrays** (composite keys) are encoded element by element. `Reading:['sensor-1', <datetime>]` therefore sorts by sensor, then by time, which makes a per-sensor time-range scan a single range scan ([13 — Key-Value & Time Series](13-key-value-time-series.md)).
- **Datetime** and **Duration** are encoded as nanoseconds in the integer form.
- `Vector` and `Geometry` values cannot be part of a key.

Record keys are built from a `RecordId` (`table` + `key`); see [03 — Data Model](03-data-model.md).

### Record store and edge store (Implemented — unified record model Phase 2)

`RecordStore` and `RecordTx` ([`crates/atlantis-storage/src/records.rs`](../crates/atlantis-storage/src/records.rs)) store records and edges on `atlantis-kv`. A `RecordTx` works in one namespace and database. Its method list is in [21 — API Reference](21-api-reference.md#record-transactions).

- **Tables.** A table is created implicitly, schemaless, on its first write, or explicitly with `define_table`. A table is either `Normal` or `Edge { from, to }`, where non-empty `from` / `to` lists restrict the endpoint tables. Field schemas are Phase 2 ([07 — Schema & DDL](07-schema-ddl.md)).
- **Nodes.** A node is a record under `r/` in the table named by its first label. A node without labels goes into the reserved table `_default`. Secondary labels are stored with the record and get one `lb/` entry each, so a label lookup is a prefix scan.
- **Relationships.** A relationship is a record in its edge table (for example `WORKS_AT`) with `in` / `out` RecordIds. Creating it writes three keys in one transaction: the edge record, an outgoing adjacency entry under the start node, and an incoming adjacency entry under the end node. Both traversal directions are therefore prefix scans. Both endpoints must exist and must not be edges. `relate` also rewrites both endpoint records unchanged, so a concurrent delete of an endpoint fails with `TxConflict` instead of leaving a dangling edge.
- **Deletes.** Deleting a node that still has edges fails with `SchemaViolation` unless `detach` is set, in which case its `g/` entries are scanned and the edges are deleted in the same transaction.
- **Values.** Stored values are bincode with a leading format byte, so the format can evolve.

Today's Cypher graph store is not on this layout yet. Moving it, and generalising the layout to typed fields and secondary indexes, is open work ([07 — Schema & DDL](07-schema-ddl.md), [08 — Document & Relational](08-document-relational.md)).

### Backends

| Backend | URL | Use | Status |
|---|---|---|---|
| `mem` | `mem://` (URL Phase 1) | Tests, ephemeral and small embedded databases | Implemented (`MemStore`, no URL selection yet) |
| `rocksdb` | `rocksdb://path` | Default persistent backend, embedded and single-node server | Phase 1 |
| `tikv` | `tikv://pd1:2379,pd2:2379` | Distributed cluster of stateless compute nodes | Phase 7 |

- **`mem`** ([`crates/atlantis-kv/src/mem.rs`](../crates/atlantis-kv/src/mem.rs)) keeps an ordered in-memory map with MVCC versions and prunes versions that no open snapshot can read. It has no write-ahead log. It can write its latest committed state to a single checkpoint file atomically (checkpoints are serialized, written to a unique temporary file, fsynced, renamed, and then the directory is fsynced) and load it back. `Engine` uses this file, `records.kv` in the database directory, on `checkpoint()`, on `shutdown()`, and when the last `Engine` clone is dropped. Commits after the last checkpoint are lost on a crash. `Engine::open(dir)` also takes an exclusive `LOCK` file, so only one engine can open a directory at a time.
- **`rocksdb`** is an LSM-tree with a write-ahead log. Its sync mode controls the durability window ([05 — Transactions & Consistency](05-transactions-consistency.md)).
- **`tikv`** provides Raft replication and distributed transactions; see [20 — Distributed Architecture](20-distributed-architecture.md).

### CSR as an adjacency cache (Planned — Phase 1)

CSR + delta stays, but it is no longer the source of truth. It becomes an in-memory **adjacency cache** rebuilt from `g/` keys:

- On first traversal of an edge table, the cache is filled by scanning its `g/` entries for both directions.
- Committed edge writes are appended to the delta, and the existing compaction rule folds them into the CSR.
- Each cache entry records the commit version it reflects. A reader whose snapshot is older than the cache, or a transaction with uncommitted edge writes, falls back to scanning `g/` keys so it still sees exactly its own snapshot.
- In a cluster, each node keeps its own cache and checks versions against KV before use ([20 — Distributed Architecture](20-distributed-architecture.md)).

The hot/cold tiering logic becomes **cache management**: the memory budget bounds cached records and adjacency, and eviction drops cache entries instead of moving data to a separate file. The backend owns persistence.

### Migration path

1. **Phase 0** fixes today's store: stable-ID promotion, edge cleanup on `DETACH DELETE`, and HNSW persistence ([25 — Implementation Roadmap](25-implementation-roadmap.md)).
2. **Phase 1** introduces `atlantis-kv` with `mem` (done) and `rocksdb`, moves Cypher's nodes, edges, and adjacency onto the key layout above (the record store already uses it), and turns CSR into the adjacency cache. Today's hot/cold tier and `checkpoint.db` format remain available until the migration completes, and a one-time import reads a `checkpoint.db` (format version 1) into a `rocksdb://` database.
3. **Phase 2** extends the layout to typed tables and secondary indexes (`ix/`). Namespaces, databases, the table catalog and the label index are already in place.
4. **Phase 3** adds `ft/`, `vx/`, `kv/`, and spatial index keys; **Phase 4** adds `cf/`; **Phase 7** adds the `tikv` backend.

---

## Design Decisions

**Why CSR + delta (today and as the target cache)?**
CSR gives contiguous neighbor slices with good cache locality, which matters for traversal-heavy queries. Rebuilding it on every insert is too expensive, so a delta log absorbs inserts until a threshold triggers compaction.

**Why move to an ordered KV layer?**
One ordered, transactional keyspace gives every model the same atomicity, isolation, and durability, and lets one backend swap (`rocksdb` to `tikv`) take the whole database from embedded to distributed. Today's design ties durability to whole-store snapshots and has no transactions, which cannot support the multi-model scope.

**Why memcomparable keys?**
They turn range queries, composite keys, time-series windows, and index lookups into prefix scans that every ordered KV backend supports natively, with no backend-specific comparators.

**Why keep both `out` and `in` adjacency entries?**
Two small extra keys per edge make traversal in either direction a prefix scan, which today's outgoing-only CSR cannot do.

**Why RocksDB as the default persistent backend?**
It is an embeddable, widely deployed LSM engine with a write-ahead log, snapshots, and tunable sync, which covers the embedded and single-node server modes without an external service.

## What's Next

- **[05 — Transactions & Consistency](05-transactions-consistency.md)** — MVCC, isolation levels, and durability on top of `atlantis-kv`.
- **[10 — Vector Index](10-vector-index.md)** — HNSW today and its persistence through `vx/` keys.
- **[15 — Query Planner & Executor](15-query-planner-executor.md)** — How the executor reads from the store.
- **[20 — Distributed Architecture](20-distributed-architecture.md)** — The `tikv` backend.
- **[22 — Configuration](22-configuration.md)** — Storage settings.
