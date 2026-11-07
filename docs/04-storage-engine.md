# 04 — Storage Engine

> **Status:** Partially implemented — `atlantis-kv` with the `mem` backend, write-ahead log, checkpoints, key encoding and the record store (used by Cypher and the Rust record API) are implemented; `rocksdb`, the `interval` sync mode and the adjacency cache Planned — Phase 1, secondary indexes Phase 2, `tikv` Phase 7
> **Crates:** `atlantis-storage`, `atlantis-kv`, `atlantis-core`

This chapter covers how Atlantis DB stores data: the `atlantis-kv` ordered key-value
layer, its memcomparable key encoding, the `mem` / `rocksdb` / `tikv` backends, the
write-ahead log that makes `mem` durable, and the record and edge store on top. There is
one store: Cypher (`Engine::execute`) and record transactions
([21 — API Reference](21-api-reference.md)) read and write the same records.

Transactions, isolation, and durability guarantees are covered in [05 — Transactions & Consistency](05-transactions-consistency.md).

---

## Transactional KV layer and record store

The target storage stack separates **what** is stored (records, edges, indexes) from **where** bytes live (a pluggable transactional KV backend):

```
 atlantis-storage ── record store, edge store, adjacency and record caches (planned)
          │
 atlantis-kv ─────── KvStore / KvTransaction traits, key encoding, MVCC
          │
   backends: mem | rocksdb (default persistent) | tikv (distributed, Phase 7)
```

Every model (graph, document, relational, key-value, time-series, vector, full-text, geospatial) is stored as ordered keys in one keyspace. Indexes, change feeds, and the catalog live in the same keyspace and are written in the same transaction as the data they describe.

Implemented today: `atlantis-kv` with the `mem` backend and its write-ahead log, the record, catalog, adjacency and label keys, and the record store. Planned: the `rocksdb` and `tikv` backends, the adjacency cache, and the secondary-index `ix/`, `ft/`, `vx/`, `cf/` and `kv/` keys.

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
| `/sys/ns/{ns}/db/{db}/ix/{name}` | Index definition (today: `VectorIndexDef`) | Implemented |
| `/ns/{ns}/db/{db}/tb/{tb}/r/{key}` | A record (node, edge, document, row, time-series point) | Implemented |
| `.../tb/{tb}/g/{nodeKey}/{dir}/{edgeTable}{edgeKey}` | An adjacency entry under the node's table; `dir` is `o` (outgoing) or `i` (incoming) | Implemented |
| `/ns/{ns}/db/{db}/lb/{label}/{table}{key}` | A secondary label index entry | Implemented |
| `.../tb/{tb}/ix/{index}/{encodedValues}/{key}` | A secondary index entry | Phase 2 |
| `.../tb/{tb}/ft/{index}/...` | Full-text postings and statistics | Phase 3 |
| `.../tb/{tb}/vx/{index}/...` | HNSW graph layers and vectors | Phase 3 |
| `.../cf/{tb}/{versionstamp}` | A change-feed entry | Phase 4 |
| `/ns/{ns}/db/{db}/kv/d/{key}` | A raw key-value entry (see [13 — Key-Value & Time-Series](13-key-value-time-series.md)) | Phase 3 |
| `/ns/{ns}/db/{db}/kv/x/{expiresAt}/{key}` | TTL expiry queue entry, scanned by the expiry worker | Phase 3 |
| `/sys/...` (other) | Fields, users, cluster membership | Phases 2, 5, 7 |

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

### Record store and edge store (Implemented — schema and indexes Phase 2)

`RecordStore` and `RecordTx` ([`crates/atlantis-storage/src/records.rs`](../crates/atlantis-storage/src/records.rs)) store records and edges on `atlantis-kv`. A `RecordTx` works in one namespace and database. Its method list is in [21 — API Reference](21-api-reference.md#record-transactions).

- **Tables.** A table is created implicitly, schemaless, on its first write, or explicitly with `define_table`. A table is either `Normal` or `Edge { from, to }`, where non-empty `from` / `to` lists restrict the endpoint tables. Field schemas are Phase 2 ([07 — Schema & DDL](07-schema-ddl.md)).
- **Nodes.** A node is a record under `r/` in the table named by its first label. A node without labels goes into the reserved table `_default`. Secondary labels are stored with the record and get one `lb/` entry each, so a label lookup is a prefix scan. `set_labels` adds and removes secondary labels; a record's table label cannot be removed.
- **Relationships.** A relationship is a record in its edge table (for example `WORKS_AT`) with `in` / `out` RecordIds. Creating it writes three keys in one transaction: the edge record, an outgoing adjacency entry under the start node, and an incoming adjacency entry under the end node. Both traversal directions are therefore prefix scans. Both endpoints must exist and must not be edges. `relate` also rewrites both endpoint records unchanged, so a concurrent delete of an endpoint fails with `TxConflict` instead of leaving a dangling edge.
- **Deletes.** Deleting a node that still has edges fails with `SchemaViolation` unless `detach` is set, in which case its `g/` entries are scanned and the edges are deleted in the same transaction.
- **Values.** Stored values are bincode with a leading format byte, so the format can evolve.
- **Vector index definitions.** `define_vector_index`, `vector_index(es)` and `drop_index` keep `VectorIndexDef` entries in the catalog. The HNSW graph itself is derived data held in memory by the engine ([10 — Vector Index](10-vector-index.md)).
- **Commit info.** `RecordTx::commit` returns a `CommitInfo` with the commit's `Versionstamp` and the ids of every record and edge the transaction wrote or deleted. The engine uses it to keep vector indexes current.

Cypher runs on this layout today. Generalising it to typed fields and secondary indexes is Phase 2 ([07 — Schema & DDL](07-schema-ddl.md), [08 — Document & Relational](08-document-relational.md)).

### Backends

| Backend | URL | Use | Status |
|---|---|---|---|
| `mem` | `mem://` (URL Phase 1) | Tests, ephemeral and small embedded databases | Implemented (`MemStore`, no URL selection yet) |
| `rocksdb` | `rocksdb://path` | Default persistent backend, embedded and single-node server | Phase 1 |
| `tikv` | `tikv://pd1:2379,pd2:2379` | Distributed cluster of stateless compute nodes | Phase 7 |

- **`mem`** ([`crates/atlantis-kv/src/mem.rs`](../crates/atlantis-kv/src/mem.rs)) keeps an ordered in-memory map with MVCC versions and prunes versions that no open snapshot can read. An engine with a `disk_path` opens it durably with a write-ahead log (next section); an in-memory engine writes nothing to disk.
- **`rocksdb`** is an LSM-tree with its own write-ahead log. Its sync mode controls the durability window ([05 — Transactions & Consistency](05-transactions-consistency.md)).
- **`tikv`** provides Raft replication and distributed transactions; see [20 — Distributed Architecture](20-distributed-architecture.md).

### Write-ahead log and checkpoints (Implemented — `mem` backend)

A database directory holds:

| File | Holds |
|---|---|
| `records.kv` | The latest checkpoint: every live key and value at one commit version |
| `records.wal` | The active log, one record per committed transaction that wrote something |
| `records.wal.old` | The previous log while a checkpoint is in progress, or after a checkpoint crashed before deleting it |
| `LOCK` | An exclusive file lock, so only one engine opens the directory at a time |

The frame format and the full protocol are documented in
[`crates/atlantis-kv/src/wal.rs`](../crates/atlantis-kv/src/wal.rs) and
[`mem.rs`](../crates/atlantis-kv/src/mem.rs). In short:

- **Commit.** After conflict validation, and while holding the store-wide write lock, a
  commit appends its log record (and, with `SyncMode::Every`, syncs it) before the
  writes are applied in memory and the commit returns. If the append fails, the commit
  fails and nothing is applied. Cancelled and conflicting transactions never reach the
  log.
- **Checkpoint.** The log is rotated to `records.wal.old` and the latest state copied
  under the lock; `records.kv` is then written atomically without the lock, and
  `records.wal.old` is deleted. Checkpoints run every `checkpoint_interval_secs`
  (default 300) on a background thread, on `Engine::checkpoint()` and
  `Engine::shutdown()`, and when the last `Engine` clone is dropped.
- **Recovery.** Opening loads `records.kv`, then replays `records.wal.old` and
  `records.wal`, skipping records the checkpoint already covers. Replay of a file stops
  at the first torn or corrupt record, or at a version gap, and the file is truncated
  there. The recovered state is always a prefix of the commit history, and every commit
  whose log record reached disk is recovered.

Opening a directory that holds only a legacy `checkpoint.db` (the format of the removed
hot/cold graph store) fails with an error; there is no importer.

### Adjacency cache (Planned — Phase 1)

Traversal today is a prefix scan of `g/` keys in the transaction's snapshot. The planned
in-memory **adjacency cache** is derived from those keys and never the source of truth:

- On first traversal of an edge table, the cache is filled by scanning its `g/` entries for both directions.
- Each cache entry records the commit version it reflects. A reader whose snapshot is older than the cache, or a transaction with uncommitted edge writes, falls back to scanning `g/` keys so it still sees exactly its own snapshot.
- In a cluster, each node keeps its own cache and checks versions against KV before use ([20 — Distributed Architecture](20-distributed-architecture.md)).

A memory budget for cached records and adjacency belongs to this cache; the backend owns persistence.

### Remaining storage work

1. **Phase 1** adds the `rocksdb` backend, backend features and connection URLs, and the adjacency cache. The legacy hot/cold graph store is already gone: Cypher runs on the record layout above.
2. **Phase 2** extends the layout to typed tables and secondary indexes (`ix/`). Namespaces, databases, the table catalog and the label index are already in place.
3. **Phase 3** adds `ft/`, `vx/`, `kv/`, and spatial index keys; **Phase 4** adds `cf/`; **Phase 7** adds the `tikv` backend.

---

## Design Decisions

**Why move to an ordered KV layer?**
One ordered, transactional keyspace gives every model the same atomicity, isolation, and durability, and lets one backend swap (`rocksdb` to `tikv`) take the whole database from embedded to distributed. The removed hot/cold graph store tied durability to whole-store snapshots and had no transactions, which could not support the multi-model scope.

**Why memcomparable keys?**
They turn range queries, composite keys, time-series windows, and index lookups into prefix scans that every ordered KV backend supports natively, with no backend-specific comparators.

**Why keep both `out` and `in` adjacency entries?**
Two small extra keys per edge make traversal in either direction a prefix scan.

**Why a write-ahead log on the `mem` backend?**
It makes every acknowledged commit durable before the `rocksdb` backend exists, without changing the transaction model: the log holds whole committed transactions in commit order, so recovery replays a prefix of history and never a partial transaction.

**Why RocksDB as the default persistent backend?**
It is an embeddable, widely deployed LSM engine with a write-ahead log, snapshots, and tunable sync, which covers the embedded and single-node server modes without an external service.

## What's Next

- **[05 — Transactions & Consistency](05-transactions-consistency.md)** — MVCC, isolation levels, and durability on top of `atlantis-kv`.
- **[10 — Vector Index](10-vector-index.md)** — HNSW indexes derived from records, and their planned persistence through `vx/` keys.
- **[15 — Query Planner & Executor](15-query-planner-executor.md)** — How the executor reads from the store.
- **[20 — Distributed Architecture](20-distributed-architecture.md)** — The `tikv` backend.
- **[22 — Configuration](22-configuration.md)** — Storage settings.
