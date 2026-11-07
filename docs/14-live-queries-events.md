# 14 — Live Queries & Events

> **Status:** Planned — Phase 4
> **Crates:** `atlantis-engine`, `atlantis-cypher`, `atlantis-kv` (planned), `atlantis-server` (planned), `atlantis-db`

Atlantis DB becomes reactive through three related features:

| Feature | Question it answers | Delivery |
|---|---|---|
| **Change feed** | "What changed in this table since position X?" | Pull, durable, replayable |
| **Live query** | "Tell me as soon as the result of this query changes." | Push, while the session is open |
| **Event** | "When this kind of write happens, also do that." | Runs inside the writing transaction |

None of these exist today. The engine (`crates/atlantis-engine/src/datastore.rs`) runs
queries and record transactions on the caller's task and has no notification or
subscription path. Commits already report the record ids they changed (`CommitInfo`),
which today only keeps vector indexes current. All three features build on the
transactional storage layer (implemented on `mem`) and the table model (Phase 2).

## Change Feeds (Planned — Phase 4)

### Enabling

```cypher
ALTER TABLE Order CHANGEFEED 7d;     // keep 7 days of changes
ALTER TABLE Order CHANGEFEED NONE;   // disable and discard the feed
```

The duration is the retention of feed entries, not of the data. See
[07 — Schema & DDL](07-schema-ddl.md) for the full table grammar.

### Reading

```cypher
CALL db.changes('Order', {since: $versionstamp}) YIELD change
RETURN change;

CALL db.changes('Order', {since: $versionstamp, limit: 500}) YIELD change
RETURN change.versionstamp, change.action, change.id;
```

Each `change` is a map:

| Field | Description |
|---|---|
| `versionstamp` | Position of the commit that made the change. Pass the last one you processed as `since` to resume. |
| `action` | `'CREATE'`, `'UPDATE'` or `'DELETE'` |
| `id` | RecordId of the changed record |
| `record` | The record after the change. For `DELETE`, the record as it was before deletion. |

`since` is exclusive: `db.changes` returns changes after that versionstamp, in commit
order. Omitting it starts from the oldest retained entry. Asking for a position older
than the retention window fails with an error, so a consumer knows it has missed
changes and must resynchronize from a full read.

### Versionstamps

A versionstamp is an opaque, totally ordered value that identifies a commit within a
database. On a single node it comes from the commit sequencer
([16 — Engine Runtime](16-engine-runtime.md)). On a cluster it is the commit timestamp of
the distributed transaction ([20 — Distributed Architecture](20-distributed-architecture.md)).
All changes from one transaction share a versionstamp. Clients must treat it as a
value to store and pass back, not as a number to do arithmetic on.

### Storage

Feed entries are written in the same transaction as the record change, under the
`cf/` prefix ([04 — Storage Engine](04-storage-engine.md)):

```
/ns/{ns}/db/{db}/cf/{tb}/{versionstamp}   -> list of changes from that commit
```

Entries are ordered by versionstamp, so reading a feed is one range scan. A background
worker removes entries older than the configured retention.

## Live Queries (Planned — Phase 4)

### Starting and Stopping

```cypher
LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o;   // returns live id
KILL LIVE $liveId;
```

`LIVE` in front of a query registers it and immediately returns one row with the live
query ID (a UUID). It does not return the current result; run the same query without
`LIVE` first if you need an initial snapshot. Notifications then arrive on the session
that registered the query until `KILL LIVE`, until the session ends, or until the
session's authentication expires.

### Supported Shape

In Phase 4, a live query has the form:

```
LIVE MATCH (x:Table) [WHERE predicate] RETURN projection
```

- One node pattern (or one relationship pattern `()-[r:EDGE_TABLE]->()`) on one table.
- `WHERE` may use the record's own fields, parameters and functions without side effects.
- `RETURN` may project fields, maps and expressions of the record.
- No aggregation, `ORDER BY`, `SKIP`/`LIMIT`, multi-hop patterns or subqueries. These
  require maintaining a full result set and fail at registration with
  `AtlantisError::InvalidQuery`.

Parameters are bound once, when the query is registered.

### Notification Format

```json
{ "id": "6f1d0c9e-...", "action": "UPDATE", "record": { "o": { "id": "Order:1042", "status": "open", "total": 99.5 } } }
```

| Field | Description |
|---|---|
| `id` | The live query ID returned by `LIVE` |
| `action` | `CREATE`, `UPDATE` or `DELETE`, relative to the live result set (see below), or `KILLED` as the final notification when the server ends the live query (queue overflow, sign-out, token expiry) |
| `record` | The `RETURN` projection for the record, as a map keyed by return item |

Over WebSocket RPC the server pushes `{result: {id, action, record}}`
([17 — Server & Protocols](17-server-protocols.md)).

### Action Semantics

The action describes how the record moved relative to the live query's result set. The
`WHERE` clause is evaluated on the record before and after the write:

| Write | Matched before | Matches after | Notification |
|---|---|---|---|
| create | — | yes | `CREATE` |
| update | no | yes | `CREATE` (record entered the result) |
| update | yes | yes | `UPDATE` |
| update | yes | no | `DELETE` (record left the result), with the last matching projection |
| delete | yes | — | `DELETE`, with the last projection |
| any | no | no | none |

A client that applies these notifications to a local copy of the result therefore
keeps an exact mirror of `MATCH ... WHERE ... RETURN`.

### Permissions

Live queries obey the same record-level permissions as normal reads
([18 — Security & Auth](18-security-auth.md)):

- Registration requires `read` permission on the table.
- For every change, the table's `FOR read` rule is evaluated with the subscriber's
  `$auth` against the before and after states. A record the subscriber cannot read is
  treated as not matching, so a permission change can itself produce `CREATE` or
  `DELETE` notifications.
- When a session signs out or its token expires, its live queries are killed, and each
  receives a final `KILLED` notification if the connection is still open.
- `db.changes` follows the same rule. System users see every change. Application
  (record or JWT) users see only changes whose before or after state passes the table's
  `FOR read` rule, so the catch-up path never reveals more than live notifications would.

### Embedded Rust API

```rust
use futures::StreamExt;

let mut live = engine
    .live("MATCH (o:Order) WHERE o.status = 'open' RETURN o", Params::new())
    .await?;

println!("live id: {}", live.id());

while let Some(n) = live.next().await {
    let n = n?;                       // Notification { id, action, record }
    match n.action {
        Action::Create => println!("new open order {:?}", n.record),
        Action::Update => println!("changed {:?}", n.record),
        Action::Delete => println!("no longer open {:?}", n.record),
        Action::Killed => break,        // server ended it; catch up via db.changes
    }
}
// dropping `live` kills the live query
```

`engine.live(query, params)` takes the query without the `LIVE` keyword and returns a
stream. The same method is available on a remote `Engine`, where it maps to the RPC
`live` and `kill` methods ([19 — Client SDKs](19-client-sdks.md)). Running
`engine.execute("LIVE MATCH ...")` also works but only returns the ID; that form is for
server clients that receive notifications on the connection.

## Events (Planned — Phase 4)

An event runs Cypher automatically when a record in a table is written.

```cypher
CREATE EVENT order_shipped ON Order
  WHEN $event = 'UPDATE' AND $before.status <> 'shipped' AND $after.status = 'shipped'
  THEN { CREATE (:Notification {order: $after.id, at: time.now()}) };

DROP EVENT order_shipped ON Order;
```

| Parameter | Value |
|---|---|
| `$event` | `'CREATE'`, `'UPDATE'` or `'DELETE'` |
| `$before` | The record before the write; `null` for `CREATE` |
| `$after` | The record after the write; `null` for `DELETE` |
| `$auth`, `$session` | The identity and session of the writer |

- `WHEN` is optional; without it the event fires on every write to the table.
- `THEN { ... }` contains one or more Cypher statements. They can read and write any
  table the writer could, plus `db.kv.*` and other procedures.
- Several events on the same table run in name order.

### Execution Semantics

- Events run **inside the writing transaction**, after the record write and before
  commit. Their writes commit or roll back together with the triggering write. An error
  in an event aborts the whole transaction.
- Writes made by an event can fire further events. Nesting is limited to a fixed
  maximum depth, and exceeding it aborts the transaction. This stops accidental cycles.
- Events see the transaction's own uncommitted writes.
- Events never call external systems. For webhooks, email or queues, have a worker
  consume the change feed or a live query and act after commit.

Typical uses: audit trails, denormalized counters, rollup tables for time-series
([13 — Key-Value & Time-Series](13-key-value-time-series.md)), and cascading cleanup.

## Ordering and Delivery Guarantees (Planned — Phase 4)

| | Change feed | Live query | Event |
|---|---|---|---|
| Sees uncommitted data | No | No | Yes (own transaction) |
| Order | Commit order (versionstamp) per database | Commit order per live query | Write order within the transaction |
| Delivery | Durable until retention expires; consumer can replay from any retained position | At most once, only while the session is open | Exactly once per write, atomically with it |
| Survives restart | Yes | No; clients re-register | n/a |

- Live notifications are sent only after commit. A rolled-back transaction produces no
  notifications and no feed entries.
- A transaction that changes the same record several times produces one notification per
  live query with the final state, and one feed entry per change.
- Each live query has a bounded outgoing queue. If a subscriber falls too far behind, the
  live query is killed and the connection receives a final `KILLED` notification. Live
  delivery is therefore at most once; change feeds are the durable path. The recommended pattern
  for "never miss a change" is: read the latest versionstamp, register the live query,
  and on any gap catch up with `db.changes` from the last versionstamp you processed.

## Runtime Design (Planned — Phase 4)

```
 commit ──► commit sequencer ──► change set (versionstamp, before/after per record)
                                     │
               ┌─────────────────────┼─────────────────────┐
               ▼                     ▼                     ▼
      cf/ entries (same txn)   live-query matcher     (events already ran
                               per table:              inside the txn)
                               WHERE before/after
                               + permission check
                                     │
                                     ▼
                         per-session queues ──► Rust stream / WS push
```

- Live queries are registered in memory, indexed by table, so a commit only checks the
  live queries of the tables it touched.
- Live query registrations are also written to KV, so a server can clean up queries
  of dead sessions after a restart.
- In a cluster, each node consumes committed changes and fans notifications out to the
  nodes that hold the subscribing sessions through per-node notification queues
  ([20 — Distributed Architecture](20-distributed-architecture.md)).

## Design Decisions

**Why actions relative to the result set?** A subscriber cares about what its query
returns, not about the raw write. Translating "updated so it no longer matches" into
`DELETE` lets clients mirror a result with a simple create/update/delete handler.

**Why three features instead of one?** They have different guarantees. Change feeds are
durable and replayable but polled. Live queries are immediate but transient. Events
are transactional but must stay fast and internal. Each workload picks the guarantee it
needs.

**Why run events inside the transaction?** Derived data such as counters and rollups
then never disagrees with its source. The cost is extra write latency, which is why
events must not call external systems.

**Why no initial snapshot from `LIVE`?** Returning a snapshot and then a stream from the
same call complicates the protocol and the permission model. A plain query plus a live
query, reconciled by versionstamp or record ID, covers the same need.

## What's Next

- [17 — Server & Protocols](17-server-protocols.md) — WebSocket `live` and `kill` methods.
- [18 — Security & Auth](18-security-auth.md) — permissions applied to live results.
- [20 — Distributed Architecture](20-distributed-architecture.md) — distributed live queries.
- [13 — Key-Value & Time-Series](13-key-value-time-series.md) — rollups built with events.
- [25 — Implementation Roadmap](25-implementation-roadmap.md) — Phase 4 scope.
