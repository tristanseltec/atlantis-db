# 05 — Transactions & Consistency

> **Status:** Partially implemented — transactions on the `mem` backend (MVCC, snapshot and serializable isolation, conflicts, retry, write-ahead-log durability with group commit and sync modes `every`, `interval` and `never`, a configurable default isolation) for Cypher and the Rust record API, and `BEGIN`/`COMMIT`/`ROLLBACK` scripts, are implemented; `rocksdb` and the per-database commit sequencer Planned — Phase 1; change-feed versionstamps Phase 4
> **Crates:** `atlantis-kv`, `atlantis-storage`, `atlantis-db`, `atlantis-engine`

This chapter defines the transaction model of Atlantis DB: ACID guarantees, MVCC, isolation levels, explicit transactions in Cypher and Rust, conflict handling and retry, durability modes, and the versionstamps that change feeds use.

Cypher and the Rust record API share one store and one transaction mechanism. The first section states exactly what is guaranteed today so that applications do not assume more.

---

## Implemented today

Every Cypher statement and every Rust record transaction runs as a transaction on `atlantis-kv` with the `mem` backend ([`crates/atlantis-kv/src/mem.rs`](../crates/atlantis-kv/src/mem.rs)), on the calling task.

| Property | Today |
|---|---|
| Isolation | MVCC. Snapshot isolation by default; serializable with `transaction_with_isolation(Isolation::Serializable, ...)`. `Engine::transaction`, `transaction_with_retry` and Cypher sent through `Engine::execute` use `transaction.isolation` (snapshot unless configured). |
| Atomicity | All keys a transaction writes (records, adjacency entries, label entries, catalog entries) commit together or not at all. A closure that returns `Err`, or a Cypher statement that fails, writes nothing. |
| Consistency | Table kind (normal or edge), edge endpoint tables, existing endpoints that are not themselves edges, and detach-before-delete are checked inside the transaction (`SchemaViolation`, `RecordNotFound`, `RecordExists`). `relate` rewrites both endpoint records unchanged, so a concurrent delete of an endpoint fails with `TxConflict` instead of leaving a dangling edge. Field schemas are Phase 2. |
| Durability | An engine opened on a directory appends every committing transaction to a write-ahead log (`records.wal`) before the commit returns. With the default `SyncMode::Every` the commit waits for an fsync covering its record (shared with concurrent commits, outside the store lock), so an acknowledged commit survives a crash or power loss. With `SyncMode::Interval` a background thread fsyncs periodically. With `SyncMode::Never` flushing is left to the OS: commits survive a process crash, but recent ones can be lost if the machine fails. An in-memory engine (`Engine::open_in_memory`) is not durable. |
| Concurrent engines | `Engine::open(dir)` takes an exclusive `LOCK` file in the directory, so a second engine on the same directory fails with "already open in another engine". |
| Conflicts | First committer wins. A losing commit fails with `TxConflict`, which `is_retryable()` reports as retryable. |
| Multi-statement Cypher | A `;`-separated script runs each statement in its own transaction, unless statements are grouped in `BEGIN; ... COMMIT;`. Inside a Rust transaction, `tx.execute` runs Cypher in that transaction; a failed statement makes the whole transaction roll back. |

---

## Target design

### ACID guarantees (Partially implemented — `mem` backend; schema checks Phase 2)

Every read and write runs inside a transaction on `atlantis-kv` ([04 — Storage Engine](04-storage-engine.md)):

- **Atomicity.** All keys a transaction writes (records, adjacency entries, index entries, change-feed entries, catalog entries) commit together or not at all.
- **Consistency.** Field types, assertions, and unique constraints are checked inside the transaction before commit ([07 — Schema & DDL](07-schema-ddl.md), Phase 2). A failed check aborts the transaction.
- **Isolation.** Each transaction reads from a consistent snapshot. Snapshot isolation is the default; serializable is optional.
- **Durability.** A committed transaction survives a process crash, within the window set by the backend's sync mode. The `mem` backend gives this today through its write-ahead log; `rocksdb` gives it in Phase 1.

Index maintenance is part of the same transaction: secondary, unique and full-text index entries for a record are written with the record (Phases 2–3). Today's vector indexes are the exception: they are derived in memory and updated right after each commit ([10 — Vector Index](10-vector-index.md)). Building an index over existing data when it is first created runs as a background job ([16 — Engine Runtime](16-engine-runtime.md)).

Schema changes are stored in the catalog under `/sys/...` and written through the same transaction path, so DDL is transactional with the data written alongside it. Table and vector index definitions already work this way.

### MVCC (Implemented — `mem` backend)

Atlantis DB uses multi-version concurrency control. Writers never block readers, and readers never block writers:

- A transaction takes a **read version** at `begin`. All its reads see the database exactly as of that version, plus its own uncommitted writes.
- Writes are buffered in the transaction and become visible to others only at commit, under a new **commit version**.
- Concurrency is **optimistic**: no locks are taken while the transaction runs. Conflicts are detected at commit.

Each backend supplies the versions. `mem` keeps a version list per key, validates and stages commits one at a time under a store-wide lock (held only for validation and the log append, never for an fsync), publishes them in version order once durable, and prunes versions that no open snapshot can read. Planned: `rocksdb` uses snapshots plus the per-database commit sequencer (Phase 1), and `tikv` uses timestamps from its placement driver (Phase 7).

### Isolation levels (Implemented)

| Level | Default | Guarantees | Commit check |
|---|---|---|---|
| Snapshot isolation | Yes | No dirty reads, no non-repeatable reads, no phantoms within the snapshot, no lost updates | Write-write: fails if another transaction committed a write to any key this transaction wrote, after this transaction's read version |
| Serializable | Optional | Snapshot isolation plus no write skew; equivalent to some serial order | Write-write check plus read-set validation: fails if any key or scanned range this transaction read was changed after its read version |

**Write skew** is the anomaly snapshot isolation allows. Example: two transactions each check "at least one doctor is on call", each sees two doctors, and each takes a different doctor off call. Both commit under snapshot isolation; under serializable one of them fails with a conflict.

Choose serializable for invariants that span several records and are not covered by a unique constraint. It costs more memory (the read set is tracked) and causes more retries under contention. The level is set per transaction with `engine.transaction_with_isolation`, or for the whole engine with `transaction.isolation` (builder method `default_isolation`), which `engine.transaction`, `transaction_with_retry` and Cypher statements use ([22 — Configuration](22-configuration.md)). A transaction without writes commits without validation.

### Implicit and explicit transactions (Implemented)

**Implicit.** A statement sent without `BEGIN` runs in its own transaction and commits when it finishes. If it fails, nothing it wrote is kept. In a `;`-separated script, statements before the failing one have already committed.

**Explicit, in Cypher.** A script groups statements with `BEGIN` and `COMMIT`, or discards them with `ROLLBACK`:

```cypher
BEGIN;
MATCH (a:Account {id: $from}) SET a.balance = a.balance - $amount;
MATCH (b:Account {id: $to})   SET b.balance = b.balance + $amount;
COMMIT;
```

```cypher
BEGIN;
CREATE (:AuditEntry {action: 'dry-run', at: time.now()});
ROLLBACK;
```

If a statement inside `BEGIN ... COMMIT` fails, the whole transaction is discarded and the error is returned. A script that ends inside an open `BEGIN` is rolled back and returns an error. A nested `BEGIN`, or `COMMIT` / `ROLLBACK` without `BEGIN`, is an error. `Engine::execute` returns the result of the script's last statement.

**Explicit, in Rust.** `engine.transaction(|tx| ...)` runs a **synchronous** closure that receives `&mut Transaction`. `Transaction` dereferences to `RecordTx` for the record API and adds `execute` for Cypher, which sees the transaction's own uncommitted writes (`BEGIN`/`COMMIT` are rejected there). Returning `Ok` commits; returning `Err` discards every write. A Cypher statement that fails inside `tx.execute` poisons the transaction, because its partial writes are already in it: even if the closure handles the error and returns `Ok`, the commit fails with "a statement in this transaction failed; it was rolled back" and nothing is written. There are no savepoints yet. The method itself is `async`. Signatures are owned by [21 — API Reference](21-api-reference.md#transactions).

```rust
use atlantis_db::{Engine, Object, RecordId, Value};

let engine = Engine::open_in_memory().await?;
let (from, to): (RecordId, RecordId) = ("Account:a".parse()?, "Account:b".parse()?);

engine.transaction(|tx| {
    tx.create_node(&["Account"], Object::from([("id".into(), Value::from("a")), ("balance".into(), Value::from(100))]))?;
    tx.create_node(&["Account"], Object::from([("id".into(), Value::from("b")), ("balance".into(), Value::from(0))]))?;
    Ok(())
}).await?;

// Re-runs the whole closure on TxConflict, up to 5 attempts in total.
engine.transaction_with_retry(5, |tx| {
    let balance = |r: Option<atlantis_db::Record>| r.and_then(|r| r.get("balance")?.as_int()).unwrap_or(0);
    let a = balance(tx.get(&from)?);
    let b = balance(tx.get(&to)?);
    tx.merge(&from, Object::from([("balance".into(), Value::from(a - 10))]))?;
    tx.merge(&to, Object::from([("balance".into(), Value::from(b + 10))]))?;
    Ok(())
}).await?;
```

Inside the closure, `tx.execute("MATCH (a:Account {id: 'a'}) SET a.balance = a.balance - 10", &[])?` does the same kind of update in Cypher.

**Remote clients.** The WebSocket RPC methods `begin`, `commit`, and `cancel` (Phase 5) and the SDK `tx` objects (Phase 6) map to the same mechanism ([17 — Server & Protocols](17-server-protocols.md), [19 — Client SDKs](19-client-sdks.md)).

**Read-only transactions.** `engine.read(|tx| ...)` begins a `TxMode::ReadOnly` transaction on one snapshot. Writes in it, including Cypher write clauses run with `tx.execute`, fail with `TxReadOnly`, and it commits without a check. A Cypher statement sent through `Engine::execute` runs read-write; one without writes commits without validation.

### Conflicts and retry (Implemented)

When the commit check fails, the transaction is cancelled and the caller receives `Error::TxConflict`. No partial writes are applied. The first transaction to commit wins. `Error::is_retryable()` returns `true` for `TxConflict` only.

Guidance for callers:

- **Retry the whole transaction**, re-reading data, not just the final write. The conflicting transaction may have changed what you read.
- Use a small bounded number of attempts with jittered backoff.
- Keep transactions short. Conflict probability grows with the time between `begin` and `commit` and with the number of keys touched.
- Prefer single-statement updates (`SET a.balance = a.balance - $amount`) over read-modify-write across round trips.
- Unique constraint violations, assertion failures, and permission errors are **not** conflicts and must not be retried blindly.

`engine.transaction` and `engine.execute` do not retry. `engine.transaction_with_retry(max_attempts, f)` re-runs the closure from the start on a retryable error, up to `max_attempts` attempts in total, immediately and without backoff; add backoff in the caller if contention is high. A single Cypher statement that fails with `TxConflict` is safe to resend because it has no client-side state.

### Durability and sync modes (Implemented on `mem`; `rocksdb` Phase 1)

A commit is first written to the backend's write-ahead log: `records.wal` for `mem` today ([04 — Storage Engine](04-storage-engine.md#write-ahead-log-and-checkpoints-implemented--mem-backend)), RocksDB's own log in Phase 1. The sync mode (`StorageConfig::sync`, builder method `sync_mode`) controls when the log is flushed to stable storage:

| Mode | Behaviour | Data at risk on power loss | Status |
|---|---|---|---|
| `every` (`SyncMode::Every`, default) | Commit returns and becomes visible only after an fsync covering it; concurrent commits share one fsync (group commit) | None | Implemented |
| `interval` (`SyncMode::Interval(duration)`) | A background thread fsyncs at the interval; commits return at once | Commits within the last interval | Implemented |
| `never` (`SyncMode::Never`) | Leave flushing to the OS | Anything not yet flushed by the OS | Implemented |

**Group commit.** The store lock covers only validation and the log append. A commit
waiting for durability either joins the fsync in flight or leads the next one, which
covers every record appended so far, so throughput grows with the number of concurrent
writers while each commit waits about two fsyncs. Committed state becomes visible in
version order only once durable, and reads never wait for an fsync. Measured on an NVMe
drive with an fdatasync of about 4.8 ms (`rbench --workloads ingest --dir`):

| Durable ingest, NVMe, 1 record per transaction | Before (fsync under the lock) | Group commit |
|---|---|---|
| 1 writer | 205/s, p50 4.8 ms | 206/s, p50 4.8 ms |
| 8 writers | 204/s, p50 39 ms | 912/s, p50 9.4 ms |
| 32 writers | 204/s, p50 156 ms | 3,174/s, p50 10.0 ms |
| 64 writers | 204/s, p50 311 ms | 6,198/s, p50 10.3 ms |

One writer stays bound by the fsync latency; batch records into larger transactions for
bulk loads. Under the mixed read/write workload on disk, readers went from 801 to 4,695
QPS (p99 22.6 → 3.8 ms), since they no longer queue behind fsyncs
(`benches/results/2026-11-16-small-dir-group-commit.*`).

All modes survive a process crash and a clean exit. Checkpoints fold the log into `records.kv` to bound its size and the recovery time; durability does not depend on them. Recovery replays the log after the checkpoint and stops at the first torn or corrupt record, so the recovered state is always a prefix of the commit history. With `tikv` (Phase 7), durability comes from Raft replication and the `sync` mode does not apply. Configuration keys are in [22 — Configuration](22-configuration.md).

### Versionstamps (Implemented in `atlantis-kv`; used by change feeds in Phase 4)

Every committed write transaction receives a **versionstamp**: a monotonically increasing, unique value. Today `Versionstamp(u64)` is returned by `KvTransaction::commit` and `RecordTx::commit`. The `mem` backend keeps one counter for the whole store, shared by all databases in it; `Engine::transaction` does not return the versionstamp. In the target design it is assigned by the per-database commit sequencer (single node, Phase 1) or derived from the TiKV commit timestamp (cluster, Phase 7). Versionstamps are memcomparable, so ordering keys by versionstamp orders them by commit.

Change feeds (Phase 4) use them directly. For a table with a change feed enabled (`ALTER TABLE Order CHANGEFEED 7d;`), each committing transaction writes its changes to `.../cf/{tb}/{versionstamp}` in the same transaction as the data. A consumer reads everything committed after a known point:

```cypher
CALL db.changes('Order', {since: $versionstamp}) YIELD change;
```

Because change-feed entries are written atomically with the data, a consumer never sees a change that was rolled back and never misses one that committed. Live queries and events build on the same committed-change stream ([14 — Live Queries & Events](14-live-queries-events.md)).

---

## Design Decisions

**Why snapshot isolation by default, with serializable optional?**
Snapshot isolation never blocks readers, matches what most applications expect, and keeps conflict rates low. Serializable is available for invariants that need it, without imposing read-set tracking on every workload.

**Why optimistic concurrency instead of locks?**
It has no deadlocks and no lock manager, and it maps directly onto the commit models of RocksDB-backed sequencing and TiKV. The cost is retries under heavy contention on the same keys, which the retry guidance addresses.

**Why fail the whole transaction on a statement error?**
Partial transactions are a common source of corrupted application state. A transaction either fully applies or fully disappears.

**Why one commit sequencer per database?**
It replaces today's store-wide staging lock (which no longer covers fsyncs) with a component that only orders commits per database, while reads and query execution run concurrently on snapshots. Ordering per database also gives versionstamps a simple meaning ([16 — Engine Runtime](16-engine-runtime.md)).

## What's Next

- **[04 — Storage Engine](04-storage-engine.md)** — The `atlantis-kv` layer and key encoding.
- **[14 — Live Queries & Events](14-live-queries-events.md)** — Change feeds, live queries, and events built on versionstamps.
- **[16 — Engine Runtime](16-engine-runtime.md)** — Commit sequencer and concurrent snapshot readers.
- **[21 — API Reference](21-api-reference.md)** — `engine.transaction` and error types.
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — Phase 1 scope.
