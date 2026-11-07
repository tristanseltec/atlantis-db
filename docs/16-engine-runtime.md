# 16 — Engine Runtime

> **Status:** Partially implemented — single-writer actor today; commit sequencer, concurrent readers, and background workers Planned — Phase 0 to Phase 4
> **Crates:** `atlantis-engine`, `atlantis-db`, `atlantis-kv` (planned), `atlantis-server` (planned)

This chapter describes how the engine runs: who owns state, how requests reach it, and what runs in the background. It first documents today's tokio actor, then the target runtime with a per-database commit sequencer, concurrent snapshot readers, sessions, background workers, and live-query and event dispatch.

---

## Implemented today

### Actor model

All mutable state is owned by one tokio task, the **core engine actor**. Callers hold cheap clones of a handle and send it commands over a bounded channel. Commands are processed one at a time, so there are no locks and no data races.

```
  Engine (clone) ─┐
  Engine (clone) ─┼─► EngineHandle (mpsc::Sender, capacity 128)
  Engine (clone) ─┘            │ Command + oneshot reply
                               ▼
                    CoreEngineActor (one tokio task)
                      GraphStore
                      HnswIndex
                      QueryExecutor (created per query)
```

### CoreEngineActor

`CoreEngineActor` (`crates/atlantis-engine/src/actor.rs`) owns a `GraphStore`, an `HnswIndex`, and the `EngineConfig`.

- **Startup.** If `storage.disk_path` is set, the store is opened from that directory, restoring `checkpoint.db` when present; otherwise an in-memory store is created. An empty `HnswIndex` is always created from the vector config ([04 — Storage Engine](04-storage-engine.md)).
- **Run loop.** `run()` receives commands until the channel closes or a `Shutdown` arrives. A `Query` command parses, plans, and executes the query synchronously on the actor task ([15 — Query Planner & Executor](15-query-planner-executor.md)).
- **Shutdown.** If a disk path is configured, the store is checkpointed before the loop exits. A failed checkpoint on shutdown is ignored.
- **Spawning.** `spawn_engine(config)` builds the actor, creates `mpsc::channel(128)`, starts it with `tokio::spawn`, and returns an `EngineHandle`.

### Command protocol

`Command` (`command.rs`) has five variants, each carrying a `oneshot::Sender` for its reply:

| Command | Effect |
|---|---|
| `Query { cypher, params }` | Parse, plan, and execute; reply with `QueryResult { columns, rows }` |
| `CreateNode { labels, properties }` | Insert a node directly, bypassing Cypher |
| `GetNode { id }` | Read a node from the hot tier |
| `Checkpoint` | Write `checkpoint.db` |
| `Shutdown` | Checkpoint if persistent, reply, and stop |

### EngineHandle and Engine

`EngineHandle` (`handle.rs`) is `Clone` and wraps the channel sender. Each method sends a command and awaits the reply. If the actor has stopped, the call returns `AtlantisError::Shutdown`.

The public `Engine` in `atlantis-db` (`crates/atlantis-db/src/engine.rs`) wraps the handle and exposes `open`, `open_in_memory`, `open_with_config`, `execute`, `checkpoint`, and `shutdown`. `execute` converts its `&[(&str, Value)]` parameters to a map and wraps the result in a `ResultSet`. `create_node` and `get_node` exist on the handle but not on `Engine`. See [21 — API Reference](21-api-reference.md).

### Background actors

`tiering_actor.rs` and `checkpoint_actor.rs` define `run_tiering_loop` (60 s interval) and `run_checkpoint_loop` (300 s interval). Both loops only tick; they do no work, and nothing spawns them. As a result:

- No eviction happens at runtime, because nothing calls `GraphStore::maybe_evict()`.
- `checkpoint_interval_secs` can be set through the config builder but is not used. Checkpoints happen only on `engine.checkpoint()` and on shutdown.

### Concurrency properties

- **Serial execution.** Every query, read or write, runs on the one actor task, in arrival order. Reads cannot run in parallel with each other or with writes.
- **Blocking work on the actor.** Parsing, planning, execution, and checkpoint I/O run synchronously inside the actor's async task, so a long query or a large checkpoint occupies a tokio worker thread for its duration.
- **Backpressure.** When 128 commands are queued, `send().await` waits.
- **No atomicity.** Serial execution isolates queries from each other, but a failing query keeps its earlier writes ([05 — Transactions & Consistency](05-transactions-consistency.md)).
- **Vector index population.** The actor owns an `HnswIndex`, but no command or executor path inserts into it, so `CALL db.vector.search(...)` searches an empty index unless it is filled outside the engine ([10 — Vector Index](10-vector-index.md)).

---

## Target design

The single writer evolves into a runtime where only commit ordering is serialized and everything else runs concurrently:

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
 commit        (cf/ keys)    dispatch             (CSR, HNSW)
                  │
       Background workers: index builds, change-feed GC,
       TTL expiry, retention, statistics, cache maintenance
```

### Sessions (Planned — Phase 1, auth fields Phase 5)

A **session** is the unit of client state. The embedded `Engine` has one implicit session; the server creates one per connection ([17 — Server & Protocols](17-server-protocols.md)). A session holds:

- The current namespace and database, set by `USE acme.shop;` or the RPC `use` method. Embedded default: namespace `atlantis`, database `default` (Phase 2).
- Session parameters set with the RPC `let` / `unset` methods.
- At most one open explicit transaction ([05 — Transactions & Consistency](05-transactions-consistency.md)).
- The live queries it registered, which are killed when the session ends (Phase 4).
- Authentication state exposed to queries as `$auth`, `$session`, `$access`, and `$token` (Phase 5, [18 — Security & Auth](18-security-auth.md)).

### Concurrent snapshot readers (Planned — Phase 1)

Queries no longer run on the actor. Each query executes on a worker task inside a transaction snapshot from `atlantis-kv`:

- Read-only queries never wait for writers and run fully in parallel.
- Write queries also execute in parallel, buffering their writes, and only enter the sequencer to commit.
- CPU-heavy work (planning, large scans, HNSW search, aggregation) runs on a blocking-capable pool so it does not starve I/O tasks.
- Query cancellation and a per-query timeout are enforced at operator boundaries (configuration in [22 — Configuration](22-configuration.md)).

### Commit sequencer (Planned — Phase 1)

There is one **commit sequencer** per database. It is the direct descendant of today's core actor, reduced to ordering commits:

1. Receive a transaction's write set (and read set under serializable isolation).
2. Validate it against commits made after the transaction's read version. On conflict, reply with `Error::TxConflict`.
3. Assign the next **versionstamp** and apply the batch to the backend atomically.
4. Publish the committed change set to downstream consumers: change feed, live-query and event dispatch, and cache updates.

Commits from different databases proceed independently. With the `tikv` backend (Phase 7), ordering and validation move into TiKV's distributed transactions and the local sequencer only publishes committed changes ([20 — Distributed Architecture](20-distributed-architecture.md)).

### Cache maintenance (Planned — Phase 1)

In-memory structures are caches over KV: the CSR adjacency cache and, from Phase 3, HNSW and full-text in-memory structures. The sequencer feeds them committed changes in versionstamp order, and each cache records the version it reflects so readers with older snapshots can bypass it ([04 — Storage Engine](04-storage-engine.md)). The memory budget is enforced by evicting cache entries, which replaces today's tiering actor.

### Background workers

Background work runs in supervised tasks that use ordinary transactions, so they obey the same isolation and conflict rules as user queries.

| Worker | Job | Status |
|---|---|---|
| Checkpoint and tiering | Wire today's loops: periodic `checkpoint_interval_secs` checkpoints and budget-driven eviction on the current hot/cold store | Phase 0 |
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

Shutdown stops accepting new sessions, lets running queries finish up to a deadline, cancels open explicit transactions, drains the sequencer, flushes the backend according to its sync mode, and stops background workers. There is no separate checkpoint step once the database runs on `atlantis-kv`.

---

## Design Decisions

**Why a single-writer actor today?**
It removes locks and data races with almost no code. For an embedded graph store serving one process it was an acceptable throughput trade-off.

**Why evolve it into a commit sequencer instead of adding locks?**
MVCC snapshots already isolate readers, so only commit ordering needs to be serialized. Keeping one ordered point per database preserves the simplicity of the actor while letting reads and query execution scale across cores, and it gives versionstamps a total order per database.

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
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — Phase 0 background actors and later runtime phases.
