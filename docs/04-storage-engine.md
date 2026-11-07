# 04 — Storage Engine

> **Status:** Partially implemented — hot/cold tier implemented today; transactional KV layer Planned — Phase 1
> **Crates:** `atlantis-storage`, `atlantis-core`, `atlantis-kv` (planned)

This chapter covers how Atlantis DB stores data. It has two parts:

- **Implemented today** describes the graph store that ships now: an in-memory hot tier with CSR + delta adjacency, an append-only cold tier, LRU tiering, and whole-store checkpoints.
- **Target design** describes the transactional storage foundation that replaces it: the `atlantis-kv` ordered key-value layer, the memcomparable key encoding, the `mem` / `rocksdb` / `tikv` backends, and the role CSR keeps as an adjacency cache.

Transactions, isolation, and durability guarantees are covered in [05 — Transactions & Consistency](05-transactions-consistency.md).

---

## Implemented today

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

`GraphStore` is the only storage API the engine uses. Its main operations are listed below; the source file is authoritative.

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

## Target design (Planned — Phase 1)

The target storage stack separates **what** is stored (records, edges, indexes) from **where** bytes live (a pluggable transactional KV backend):

```
 atlantis-storage ── record store, edge store, CSR adjacency cache, cache management
          │
 atlantis-kv ─────── KvStore / Transaction traits, key encoding, MVCC
          │
   backends: mem | rocksdb (default persistent) | tikv (distributed, Phase 7)
```

Every model (graph, document, relational, key-value, time-series, vector, full-text, geospatial) is stored as ordered keys in one keyspace. Indexes, change feeds, and the catalog live in the same keyspace and are written in the same transaction as the data they describe.

### `atlantis-kv` (Planned — Phase 1)

`atlantis-kv` exposes an ordered, transactional key-value API with snapshot reads:

| Operation | Meaning |
|---|---|
| `begin(read_only)` | Start a transaction on a consistent snapshot |
| `get(key)` | Read one key as of the snapshot, including the transaction's own writes |
| `set(key, value)` / `delete(key)` | Buffer a write in the transaction |
| `scan(range)` | Iterate keys in byte order within a range |
| `commit()` | Validate and atomically apply all writes |
| `cancel()` | Discard the transaction |

An illustrative shape of the traits (names and signatures are not final):

```rust
// Planned — Phase 1. Illustrative only.
pub trait KvStore: Send + Sync {
    type Tx: Transaction;
    async fn begin(&self, read_only: bool) -> Result<Self::Tx, Error>;
}

pub trait Transaction: Send {
    async fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, Error>;
    async fn set(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error>;
    async fn delete(&mut self, key: &[u8]) -> Result<(), Error>;
    async fn scan(&mut self, range: Range<Vec<u8>>, limit: Option<usize>)
        -> Result<Vec<(Vec<u8>, Vec<u8>)>, Error>;
    async fn commit(self) -> Result<(), Error>;   // may fail with Error::TxConflict
    async fn cancel(self) -> Result<(), Error>;
}
```

Backends are selected with Cargo features `kv-mem`, `kv-rocksdb`, and `kv-tikv`, and at runtime by the connection URL.

### Key encoding (Planned — Phase 1)

Keys are **memcomparable**: comparing encoded keys byte-by-byte gives the same order as comparing the logical values. A range scan over a key prefix therefore returns records, adjacency entries, or index entries in logical order, on every backend.

| Prefix | Holds |
|---|---|
| `/ns/{ns}/db/{db}/tb/{tb}/r/{key}` | A record (node, edge, document, row, time-series point) |
| `.../tb/{tb}/g/{from}/{dir}/{edgeTable}/{edgeKey}` | An adjacency entry; `dir` is `out` or `in` |
| `.../tb/{tb}/ix/{index}/{encodedValues}/{key}` | A secondary index entry |
| `.../tb/{tb}/ft/{index}/...` | Full-text postings and statistics |
| `.../tb/{tb}/vx/{index}/...` | HNSW graph layers and vectors |
| `.../cf/{tb}/{versionstamp}` | A change-feed entry |
| `/ns/{ns}/db/{db}/kv/d/{key}` | A raw key-value entry (see [13 — Key-Value & Time-Series](13-key-value-time-series.md)) |
| `/ns/{ns}/db/{db}/kv/x/{expiresAt}/{key}` | TTL expiry queue entry, scanned by the expiry worker |
| `/sys/...` | Catalog (namespaces, databases, tables, fields, indexes), users, cluster membership |

Encoding rules for key components:

- **Integers** are written big-endian with the sign bit flipped, so negative keys sort before positive ones.
- **Floats** use the standard order-preserving bit transform.
- **Strings** and bytes are written with an escaped terminator, so `"ab"` sorts before `"abc"` and a component never bleeds into the next one.
- **UUID** and **ULID** keys are written as their 16 raw bytes. ULIDs are time-ordered, so the default auto-generated key gives insertion-ordered scans.
- **Arrays** (composite keys) are encoded element by element. `Reading:['sensor-1', d'2026-11-07T00:00:00Z']` therefore sorts by sensor, then by time, which makes per-sensor time-range scans a single prefix scan ([13 — Key-Value & Time Series](13-key-value-time-series.md)).
- **Datetime** is encoded as nanoseconds since the epoch in the integer form.

Record keys are built from a `RecordId` (`table` + `key`); see [03 — Data Model](03-data-model.md).

### Record store and edge store (Planned — Phase 1, unified record model Phase 2)

- A **node** is a record under `r/` in the table named by its first label. Secondary labels are stored with the record and indexed as a tag set.
- A **relationship** is a record in its edge table (for example `WORKS_AT`) with `in` / `out` RecordIds. Creating it writes three keys in one transaction: the edge record, an `out` adjacency entry under the start node, and an `in` adjacency entry under the end node. Both traversal directions are therefore prefix scans.
- Deleting a node with `DETACH DELETE` scans its `g/` prefix to find and delete incident edges in the same transaction.

Phase 1 migrates today's graph store onto this layout. Phase 2 generalises it to all tables with namespaces, databases, and typed fields ([07 — Schema & DDL](07-schema-ddl.md), [08 — Document & Relational](08-document-relational.md)).

### Backends

| Backend | URL | Use | Status |
|---|---|---|---|
| `mem` | `mem://` | Tests, ephemeral embedded databases | Phase 1 |
| `rocksdb` | `rocksdb://path` | Default persistent backend, embedded and single-node server | Phase 1 |
| `tikv` | `tikv://pd1:2379,pd2:2379` | Distributed cluster of stateless compute nodes | Phase 7 |

- **`mem`** keeps an ordered in-memory map with MVCC versions. Data is lost on close.
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
2. **Phase 1** introduces `atlantis-kv` with `mem` and `rocksdb`, moves nodes, edges, and adjacency onto the key layout above, and turns CSR into the adjacency cache. Today's hot/cold tier and `checkpoint.db` format remain available until the migration completes, and a one-time import reads a `checkpoint.db` (format version 1) into a `rocksdb://` database.
3. **Phase 2** extends the layout to namespaces, databases, typed tables, and secondary indexes (`ix/`).
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
