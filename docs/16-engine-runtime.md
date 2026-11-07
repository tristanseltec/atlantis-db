# 16 — Engine Runtime

> **Status:** Partially implemented — the `Datastore` runtime (optimistic transactions on the calling task, a store-wide commit lock, periodic checkpoints, derived vector indexes) is implemented; sessions, the commit sequencer, off-executor query workers and the other background workers Planned — Phase 1 to Phase 7
> **Crates:** `atlantis-engine`, `atlantis-db`, `atlantis-kv`, `atlantis-server` (planned)

This chapter describes how the engine runs: who owns state, how requests reach it, and what runs in the background. It first documents today's `Datastore`, then the target runtime with a per-database commit sequencer, query workers, sessions, background workers, and live-query and event dispatch.

---

## Implemented today

### Datastore

`Datastore` (`crates/atlantis-engine/src/datastore.rs`) owns everything an open database
needs: the `MemStore` key-value store, the `RecordStore` on top of it, the derived
vector indexes, the configuration, the directory `LOCK`, and the checkpoint thread. It
is a cheap `Arc` handle; the public `Engine` in `atlantis-db` wraps one, plus the
namespace and database that handle works in ([21 — API Reference](21-api-reference.md)).
There is no actor, no command channel and no tokio task of its own.

```
  Engine (clone, own ns/db) ─┐
  Engine (clone, own ns/db) ─┼─► Datastore (Arc)
                             │     MemStore ── records.wal / records.kv / LOCK
                             │     RecordStore
                             │     VectorIndexes (Mutex)
                             │     checkpoint thread
```

- **Open.** With a `disk_path`, the directory is locked, the checkpoint and log are
  recovered ([04 — Storage Engine](04-storage-engine.md)), and a checkpoint thread is
  started when `checkpoint_interval_secs` is above 0. A directory holding only a legacy
  `checkpoint.db` is rejected with an error. Without a `disk_path`, everything stays in
  memory.
- **Execute.** `Datastore::execute` parses a script and runs each statement in its own
  transaction, or groups `BEGIN … COMMIT` ([15 — Query Planner & Executor](15-query-planner-executor.md)).
- **Commit.** A transaction commits through `MemStore`: validation, the log append (and
  fsync under `SyncMode::Every`) and the in-memory apply happen under one store-wide
  lock. Afterwards the commit's changed record ids update the built vector indexes; a
  failure there drops the index for a rebuild and never fails the commit.
- **Poisoned transactions.** A Cypher statement that fails inside an explicit
  transaction marks it failed; committing it returns an error and writes nothing
  ([05 — Transactions & Consistency](05-transactions-consistency.md)).
- **Close.** When the last handle is dropped, the checkpoint thread is stopped and a
  final checkpoint folds the log into `records.kv` (best effort; the log already makes
  every commit recoverable). `Engine::shutdown()` checkpoints but does not close.

### Concurrency properties

- **Transactions run on the calling task.** Snapshot reads and buffered writes need no
  lock; several transactions run concurrently. Only commits are serialized, by the
  store-wide lock in `MemStore`.
- **The async API is synchronous inside.** The `async` methods of `Engine` do their work
  on the calling task before returning: query execution, the WAL fsync of every commit,
  and the first-search build of a vector index, which holds the mutex shared by all
  vector indexes. A heavy call therefore blocks its tokio worker thread. For large
  workloads, call the engine from `tokio::task::spawn_blocking` or from a dedicated
  runtime. Moving this work off the async executor is planned.
- **No query limits.** There are no per-query row or time limits; unbounded
  variable-length patterns and huge `UNWIND`s can use a lot of memory or time, and only
  `range()` is capped (10 million items). `query.timeout` is planned
  ([22 — Configuration](22-configuration.md)).
- **Checkpoints.** The checkpoint thread checkpoints every `checkpoint_interval_secs`
  and blocks commits only while the log is rotated and the
  state copied, not while `records.kv` is written.

---

## Target design

Today's runtime evolves into one where only commit ordering is serialized per database and query work runs on dedicated workers:

```
 clients: embedded Engine | atlantis-server (HTTP, WS RPC, Bolt)
                  │
            Session  (ns, db, params, auth, open transaction, live queries)
                  │
   ┌──────────────┼──────────────────────────────┐
   │      Query workers (parallel)               │
   │   parse → plan → execute on a snapshot      │
   └──────────────┬──────────────────────────────┘
                  │ write sets at commit
       Commit sequencer (one per database)
                  │ versionstamp, committed changes
   ┌──────────────┼───────────────┬──────────────────┐
   ▼              ▼               ▼                  ▼
 atlantis-kv   change feed   live-query & event   cache updates
 commit        (cf/ keys)    dispatch             (adjacency, HNSW)
                  │
       Background workers: index builds, change-feed GC,
       TTL expiry, retention, statistics, cache maintenance
```

### Sessions (Planned — Phase 2, auth fields Phase 5)

A **session** is the unit of client state. The embedded `Engine` has one implicit session per handle; the server creates one per connection ([17 — Server & Protocols](17-server-protocols.md)). A session holds:

- The current namespace and database, set by `USE acme.shop;` or the RPC `use` method. Today an `Engine` handle already keeps its own selection, set with `use_db(ns, db)`; the default is namespace `atlantis`, database `default`.
- Session parameters set with the RPC `let` / `unset` methods.
- At most one open explicit transaction ([05 — Transactions & Consistency](05-transactions-consistency.md)).
- The live queries it registered, which are killed when the session ends (Phase 4).
- Authentication state exposed to queries as `$auth`, `$session`, `$access`, and `$token` (Phase 5, [18 — Security & Auth](18-security-auth.md)).

### Query workers (Planned — Phase 1)

Snapshot transactions already let reads and writes run concurrently. The remaining step is to move query work off the caller's async task:

- Read-only queries never wait for writers and run fully in parallel (as today).
- Write queries also execute in parallel, buffering their writes, and only enter the sequencer to commit (as today, with the store-wide lock in place of the sequencer).
- CPU-heavy work (planning, large scans, HNSW search, aggregation) runs on a blocking-capable pool so it does not starve I/O tasks.
- Query cancellation and a per-query timeout are enforced at operator boundaries (configuration in [22 — Configuration](22-configuration.md)).

### Commit sequencer (Planned — Phase 1)

There is one **commit sequencer** per database. It replaces today's store-wide commit lock and only orders commits:

1. Receive a transaction's write set (and read set under serializable isolation).
2. Validate it against commits made after the transaction's read version. On conflict, reply with `Error::TxConflict`.
3. Assign the next **versionstamp** and apply the batch to the backend atomically.
4. Publish the committed change set to downstream consumers: change feed, live-query and event dispatch, and cache updates.

Commits from different databases proceed independently. With the `tikv` backend (Phase 7), ordering and validation move into TiKV's distributed transactions and the local sequencer only publishes committed changes ([20 — Distributed Architecture](20-distributed-architecture.md)).

### Cache maintenance (Partially implemented — vector indexes; adjacency cache Phase 1)

In-memory structures are caches over KV: the planned adjacency cache, today's HNSW vector indexes, and, from Phase 3, full-text structures. Today vector indexes are updated from each commit's changed record ids after the commit. In the target, the sequencer feeds caches committed changes in versionstamp order, and each cache records the version it reflects so readers with older snapshots can bypass it ([04 — Storage Engine](04-storage-engine.md)). A memory budget is enforced by evicting cache entries.

### Background workers

Background work runs in supervised tasks that use ordinary transactions, so they obey the same isolation and conflict rules as user queries.

| Worker | Job | Status |
|---|---|---|
| Checkpoint | Fold the write-ahead log into `records.kv` every `checkpoint_interval_secs` (a plain thread today, not a transaction) | Implemented |
| Index builder | Build a new secondary, vector, full-text, or spatial index over existing records in batches, while new writes maintain it transactionally; the index becomes usable when the build catches up | Phase 2 (B-tree), Phase 3 (search indexes) |
| Statistics | Refresh value distributions used by the cost-based optimizer ([15 — Query Planner & Executor](15-query-planner-executor.md)) | Phase 2 |
| TTL expiry | Delete key-value entries whose TTL has passed (`db.kv.set(..., {ttl: '1h'})`) | Phase 3 |
| Retention and downsampling | Delete time-series points older than the table's `RETENTION` and run downsampling ([13 — Key-Value & Time Series](13-key-value-time-series.md)) | Phase 3 |
| Change-feed GC | Delete `cf/` entries older than the table's `CHANGEFEED` duration | Phase 4 |
| Cluster heartbeat | Register the node and renew its heartbeat in `/sys/cluster` | Phase 7 |

Reads of expired-but-not-yet-deleted KV entries treat them as absent, so TTL correctness does not depend on how fast the worker runs.

### Live-query and event dispatch (Planned — Phase 4)

The dispatcher consumes committed change sets from the sequencer, in versionstamp order:

- **Events.** For each changed record in a table with `CREATE EVENT ... ON Table`, the dispatcher evaluates the `WHEN` condition with `$event`, `$before`, and `$after` bound, and runs the `THEN` block when it holds. Whether the block runs inside the triggering transaction or after it commits is specified in [14 — Live Queries & Events](14-live-queries-events.md).
- **Live queries.** For each registered `LIVE MATCH` on the changed table, the dispatcher evaluates the compiled predicate on the before and after images under the subscriber's permissions ([15 — Query Planner & Executor](15-query-planner-executor.md)) and queues a notification for the subscriber's session. The server pushes it as `{result: {id, action, record}}` over WebSocket RPC; embedded callers receive it on a stream.
- **Ordering and delivery.** Notifications for one live query arrive in commit order. Notifications only describe committed changes. A slow subscriber's queue is bounded; when it overflows, the live query is killed and the client is told to re-register and catch up from the change feed with `CALL db.changes(...)`.
- `KILL LIVE $liveId;` and session close remove the registration.

In a cluster, live-query registrations live in KV and changes fan out through per-node notification queues ([20 — Distributed Architecture](20-distributed-architecture.md)).

### Shutdown (Planned — Phase 1)

Shutdown stops accepting new sessions, lets running queries finish up to a deadline, cancels open explicit transactions, drains the sequencer, flushes the backend according to its sync mode, and stops background workers. Today closing is implicit: the last dropped handle checkpoints and releases the lock.

---

## Design Decisions

**Why optimistic transactions on the calling task today?**
MVCC snapshots isolate readers without locks, so only commits need ordering. Running work on the calling task kept the runtime small when Cypher moved onto the record store; the cost is blocking async callers, which the planned query workers remove.

**Why a commit sequencer per database instead of more locks?**
Only commit ordering needs to be serialized. One ordered point per database keeps that simple while letting reads and query execution scale across cores, and it gives versionstamps a total order per database.

**Why run background work as ordinary transactions?**
Index builds, TTL, retention, and change-feed GC then cannot violate isolation, conflict correctly with user writes, and run unchanged on the distributed backend.

**Why dispatch live queries from committed change sets?**
Clients must never be notified about changes that later roll back. Driving dispatch from the same stream as the change feed also gives a recovery path: a client that falls behind resumes from a versionstamp.

## What's Next

- **[05 — Transactions & Consistency](05-transactions-consistency.md)** — Isolation, conflicts, and versionstamps.
- **[14 — Live Queries & Events](14-live-queries-events.md)** — User-facing semantics of live queries, events, and change feeds.
- **[17 — Server & Protocols](17-server-protocols.md)** — Sessions over HTTP and WebSocket RPC.
- **[21 — API Reference](21-api-reference.md)** — The public `Engine` API.
- **[22 — Configuration](22-configuration.md)** — Engine and worker settings.
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — Runtime phases.
