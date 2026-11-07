# 05 — Transactions & Consistency

> **Status:** Partially implemented — record-store transactions on the `mem` backend (MVCC, snapshot and serializable isolation, conflicts, retry) are implemented; Cypher transactions, `BEGIN`/`COMMIT`, sync modes and the commit sequencer Planned — Phase 1; change-feed versionstamps Phase 4
> **Crates:** `atlantis-kv`, `atlantis-storage`, `atlantis-db`, `atlantis-engine`

This chapter defines the transaction model of Atlantis DB: ACID guarantees, MVCC, isolation levels, explicit transactions in Cypher and Rust, conflict handling and retry, durability modes, and the versionstamps that change feeds use.

Two stores exist today, with different guarantees. The first section states exactly what each one guarantees so that applications do not assume more. The two stores do not share data: Cypher queries do not see records written in record transactions, and record transactions do not see Cypher nodes ([04 — Storage Engine](04-storage-engine.md)).

---

## Implemented today

### Cypher queries (`Engine::execute`)

Cypher has no transactions. The behaviour comes from the single-writer actor described in [16 — Engine Runtime](16-engine-runtime.md):

| Property | Today |
|---|---|
| Isolation | Queries run one at a time on the core actor, so a query never sees another query's partial writes. This is serial execution, not a transaction mechanism. |
| Atomicity | None. A query that fails part-way keeps the writes it already made. There is no rollback. |
| Consistency | No schema, constraints, or uniqueness checks exist to enforce. |
| Durability | Writes live in memory until `engine.checkpoint()` or shutdown writes `checkpoint.db`. A crash loses everything since the last checkpoint ([04 — Storage Engine](04-storage-engine.md)). |
| Multi-statement | Not supported. `BEGIN`, `COMMIT`, and `ROLLBACK` are not parsed. |

Treat each `engine.execute(...)` call as a best-effort, non-atomic write followed by optional explicit checkpointing.

### Record transactions (`Engine::transaction` and friends)

Record transactions run on `atlantis-kv` with the in-memory `mem` backend ([`crates/atlantis-kv/src/mem.rs`](../crates/atlantis-kv/src/mem.rs)). They run on the calling task, not on the Cypher actor.

| Property | Today |
|---|---|
| Isolation | MVCC. Snapshot isolation by default; serializable with `transaction_with_isolation(Isolation::Serializable, ...)`. |
| Atomicity | All keys a transaction writes (records, adjacency entries, label entries, catalog entries) commit together or not at all. A closure that returns `Err` writes nothing. |
| Consistency | Table kind (normal or edge), edge endpoint tables, existing endpoints that are not themselves edges, and detach-before-delete are checked inside the transaction (`SchemaViolation`, `RecordNotFound`, `RecordExists`). `relate` rewrites both endpoint records unchanged, so a concurrent delete of an endpoint fails with `TxConflict` instead of leaving a dangling edge. Field schemas are Phase 2. |
| Durability | Limited to checkpoints. The record store is in memory. It is written to `records.kv` by `engine.checkpoint()`, by `engine.shutdown()`, and automatically when the last `Engine` clone is dropped. There is no write-ahead log yet, so commits made after the last checkpoint are lost on a crash. A commit returning `Ok` does not mean the data is on disk. |
| Concurrent engines | `Engine::open(dir)` takes an exclusive `LOCK` file in the directory, so a second engine on the same directory fails with "already open in another engine". |
| Conflicts | First committer wins. A losing commit fails with `TxConflict`, which `is_retryable()` reports as retryable. |
| Multi-statement Cypher | Not available. A transaction closure calls `RecordTx` methods, not Cypher. |

---

## Target design

### ACID guarantees (Partially implemented — record store on `mem`; Cypher Phase 1)

Every read and write runs inside a transaction on `atlantis-kv` ([04 — Storage Engine](04-storage-engine.md)):

- **Atomicity.** All keys a transaction writes (records, adjacency entries, index entries, change-feed entries, catalog entries) commit together or not at all.
- **Consistency.** Field types, assertions, and unique constraints are checked inside the transaction before commit ([07 — Schema & DDL](07-schema-ddl.md), Phase 2). A failed check aborts the transaction.
- **Isolation.** Each transaction reads from a consistent snapshot. Snapshot isolation is the default; serializable is optional.
- **Durability.** A committed transaction survives a process crash, within the window set by the backend's sync mode (Phase 1, with `rocksdb`). The record store does not give this today: it is durable only up to its last checkpoint (see [Implemented today](#record-transactions-enginetransaction-and-friends)).

Index maintenance is part of the same transaction: secondary, unique, full-text, and vector index entries for a record are written with the record. Building an index over existing data when it is first created runs as a background job ([16 — Engine Runtime](16-engine-runtime.md)).

Schema changes are stored in the catalog under `/sys/...` and written through the same transaction path, so DDL is transactional with the data written alongside it. Table definitions already work this way.

### MVCC (Implemented — `mem` backend)

Atlantis DB uses multi-version concurrency control. Writers never block readers, and readers never block writers:

- A transaction takes a **read version** at `begin`. All its reads see the database exactly as of that version, plus its own uncommitted writes.
- Writes are buffered in the transaction and become visible to others only at commit, under a new **commit version**.
- Concurrency is **optimistic**: no locks are taken while the transaction runs. Conflicts are detected at commit.

Each backend supplies the versions. `mem` keeps a version list per key, validates and applies commits one at a time under a store-wide lock, and prunes versions that no open snapshot can read. Planned: `rocksdb` uses snapshots plus the per-database commit sequencer (Phase 1), and `tikv` uses timestamps from its placement driver (Phase 7).

### Isolation levels (Implemented — record store)

| Level | Default | Guarantees | Commit check |
|---|---|---|---|
| Snapshot isolation | Yes | No dirty reads, no non-repeatable reads, no phantoms within the snapshot, no lost updates | Write-write: fails if another transaction committed a write to any key this transaction wrote, after this transaction's read version |
| Serializable | Optional | Snapshot isolation plus no write skew; equivalent to some serial order | Write-write check plus read-set validation: fails if any key or scanned range this transaction read was changed after its read version |

**Write skew** is the anomaly snapshot isolation allows. Example: two transactions each check "at least one doctor is on call", each sees two doctors, and each takes a different doctor off call. Both commit under snapshot isolation; under serializable one of them fails with a conflict.

Choose serializable for invariants that span several records and are not covered by a unique constraint. It costs more memory (the read set is tracked) and causes more retries under contention. Today the level is set per transaction with `engine.transaction_with_isolation`. A transaction without writes commits without validation. A database-wide default (`transaction.isolation`) is Phase 1 ([22 — Configuration](22-configuration.md)).

### Implicit and explicit transactions (Rust record transactions implemented; Cypher Phase 1)

**Implicit (Phase 1).** A query sent without `BEGIN` runs in its own transaction and commits when it finishes. If any statement fails, nothing it wrote is kept.

**Explicit, in Cypher (Phase 1).** A multi-statement query groups statements with `BEGIN` and `COMMIT`, or discards them with `ROLLBACK`:

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

If a statement inside `BEGIN ... COMMIT` fails, the whole transaction is cancelled and the error is returned.

**Explicit, in Rust (implemented for records).** `engine.transaction(|tx| ...)` runs a **synchronous** closure that receives `&mut RecordTx`. Returning `Ok` commits; returning `Err` discards every write. The method itself is `async`. An earlier draft of this design used an `async move` closure; that form is no longer the plan. Signatures are owned by [21 — API Reference](21-api-reference.md#transactions-implemented-for-records-cypher-statements-planned--phase-1).

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

Running Cypher statements inside a transaction closure is Phase 1, once Cypher runs on the record store.

**Remote clients.** The WebSocket RPC methods `begin`, `commit`, and `cancel` (Phase 5) and the SDK `tx` objects (Phase 6) map to the same mechanism ([17 — Server & Protocols](17-server-protocols.md), [19 — Client SDKs](19-client-sdks.md)).

**Read-only transactions (implemented for records).** `engine.read(|tx| ...)` begins a `TxMode::ReadOnly` transaction on one snapshot. Writes in it fail with `TxReadOnly`, and it commits without a check. Running Cypher queries without write clauses read-only automatically is Phase 1.

### Conflicts and retry (Implemented — record store)

When the commit check fails, the transaction is cancelled and the caller receives `Error::TxConflict`. No partial writes are applied. The first transaction to commit wins. `Error::is_retryable()` returns `true` for `TxConflict` only.

Guidance for callers:

- **Retry the whole transaction**, re-reading data, not just the final write. The conflicting transaction may have changed what you read.
- Use a small bounded number of attempts with jittered backoff.
- Keep transactions short. Conflict probability grows with the time between `begin` and `commit` and with the number of keys touched.
- Prefer single-statement updates (`SET a.balance = a.balance - $amount`) over read-modify-write across round trips.
- Unique constraint violations, assertion failures, and permission errors are **not** conflicts and must not be retried blindly.

`engine.transaction` does not retry. `engine.transaction_with_retry(max_attempts, f)` re-runs the closure from the start on a retryable error, up to `max_attempts` attempts in total, immediately and without backoff; add backoff in the caller if contention is high. Implicit single-query transactions (Phase 1) will be safe to retry because they have no client-side state.

### Durability and sync modes (Planned — Phase 1)

With the `rocksdb` backend, a commit is first written to RocksDB's write-ahead log. The `sync` mode controls when the log is flushed to stable storage:

| Mode | Behaviour | Data at risk on power loss |
|---|---|---|
| `every` | fsync on every commit before returning | None |
| `interval` | fsync on a timer | Commits within the last interval |
| `never` | Leave flushing to the OS | Anything not yet flushed by the OS |

All modes survive a clean process exit. The `mem` backend, which the record store uses today, has no write-ahead log and is durable only through checkpoints: `engine.checkpoint()`, `engine.shutdown()` and dropping the last `Engine` clone write its committed state to `records.kv`. A checkpoint is atomic (serialized with other checkpoints, written to a unique temporary file, fsynced, renamed, then the directory is fsynced), so a crash leaves either the previous or the new checkpoint. Commits after the last checkpoint are lost on a crash. With `tikv` (Phase 7), durability comes from Raft replication and the `sync` mode does not apply. Configuration keys are in [22 — Configuration](22-configuration.md).

This replaces today's checkpoint model: there is no separate checkpoint step once a database runs on `rocksdb`.

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
Partial transactions are the main source of corrupted application state today. A transaction either fully applies or fully disappears.

**Why one commit sequencer per database?**
It evolves today's single-writer actor into a component that only orders commits, while reads and query execution run concurrently on snapshots. Ordering per database also gives versionstamps a simple meaning ([16 — Engine Runtime](16-engine-runtime.md)).

## What's Next

- **[04 — Storage Engine](04-storage-engine.md)** — The `atlantis-kv` layer and key encoding.
- **[14 — Live Queries & Events](14-live-queries-events.md)** — Change feeds, live queries, and events built on versionstamps.
- **[16 — Engine Runtime](16-engine-runtime.md)** — Commit sequencer and concurrent snapshot readers.
- **[21 — API Reference](21-api-reference.md)** — `engine.transaction` and error types.
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — Phase 1 scope.
