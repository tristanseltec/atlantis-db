# 13 — Key-Value & Time-Series

> **Status:** Planned — Phase 3 (rollups through events need Phase 4)
> **Crates:** `atlantis-kv` (planned), `atlantis-engine`, `atlantis-cypher`, `atlantis-db`

This chapter covers two models built directly on the ordered, transactional KV layer:

- a **key-value API** for simple values with optional expiry, and
- **time-series tables** whose records are keyed by series and time, so range queries
  and retention are cheap.

Neither exists today. The public API (`crates/atlantis-db/src/engine.rs`) has no
key-value access and no time-series table type. The foundations are in place:
`atlantis-kv` with composite array keys and `RecordTx::scan_range` for key windows,
and the `Datetime` and `Duration` values with `datetime()`, `duration()` and
`time.now()` in Cypher.

## Key-Value API (Planned — Phase 3)

The key-value API stores a `Value` under a string key in the current database. It is
meant for data that has no relationships or schema: caches, feature flags, counters,
idempotency tokens, rate-limit windows and session state.

### From Atlantis Cypher

```cypher
CALL db.kv.set('session:9f2c', {user: 'alice', cart: 3}, {ttl: '1h'});
CALL db.kv.get('session:9f2c') YIELD value RETURN value.user;
CALL db.kv.delete('session:9f2c');

// Prefix scan, in key order
CALL db.kv.scan('session:') YIELD key, value RETURN key, value;

// Paged prefix scan
CALL db.kv.scan('session:', {after: $lastKey, limit: 100}) YIELD key, value
RETURN key, value;

// Half-open range [start, end)
CALL db.kv.range('2026-11-01', '2026-11-08', {reverse: true, limit: 50})
YIELD key, value
RETURN key, value;
```

| Procedure | Yields | Description |
|---|---|---|
| `db.kv.set(key, value, options?)` | — (void) | Insert or overwrite. Options: `ttl` (duration string or `duration(...)`) |
| `db.kv.get(key)` | `value` | Returns `null` when the key is missing or expired |
| `db.kv.delete(key)` | `deleted` (bool) | Removes the key |
| `db.kv.scan(prefix, options?)` | `key, value` | Keys starting with `prefix`. Options: `after`, `limit`, `reverse` |
| `db.kv.range(start, end, options?)` | `key, value` | Keys in `[start, end)`. Options: `limit`, `reverse` |

Values can be any `Value`, including nested objects, vectors and RecordIds. Keys are
UTF-8 strings compared byte-wise, so design keys with ordering in mind: zero-pad numbers
and use ISO-8601 timestamps.

KV calls take part in the surrounding transaction like any other write
(see [05 — Transactions & Consistency](05-transactions-consistency.md)). That makes
patterns such as "create the order and record the idempotency key" atomic:

```cypher
BEGIN;
CALL db.kv.get('idem:' + $requestId) YIELD value
WITH value WHERE value IS NULL
CREATE (o:Order {total: $total})
CALL db.kv.set('idem:' + $requestId, id(o), {ttl: '24h'});
COMMIT;
```

### From Rust

The same operations are available on the embedded and remote `Engine`
([21 — API Reference](21-api-reference.md)):

```rust
use std::time::Duration;

let kv = engine.kv();
kv.set("flag:new-ui", Value::Bool(true)).await?;
kv.set_with_ttl("session:9f2c", session_value, Duration::from_secs(3600)).await?;

if let Some(v) = kv.get("flag:new-ui").await? {
    println!("{v:?}");
}

let page = kv.scan("session:").limit(100).collect().await?;   // Vec<(String, Value)>
kv.delete("session:9f2c").await?;
```

Each call outside a transaction is its own transaction. Inside
`engine.transaction(|tx| ...)`, use `tx.kv()` to join the enclosing transaction.

### TTL and Expiry

- A TTL is stored with the value as an absolute expiry time.
- Reads check the expiry, so an expired key is invisible immediately, even before it is
  physically removed.
- A background TTL worker ([16 — Engine Runtime](16-engine-runtime.md)) walks an expiry
  index in time order and deletes expired entries in small batches.
- Setting a key again without `ttl` removes its expiry.

### Storage

KV entries live in a dedicated keyspace per database, next to the table keyspaces
described in [04 — Storage Engine](04-storage-engine.md):

```
/ns/{ns}/db/{db}/kv/d/{key}                  -> value + optional expiry
/ns/{ns}/db/{db}/kv/x/{expiresAt}/{key}      -> (empty) expiry index
```

Because keys keep their order, `scan` and `range` are single KV range scans.

### When to Use KV vs. a Table

| Use KV when | Use a table when |
|---|---|
| Access is always by exact key or key prefix | You filter on fields other than the key |
| Values are opaque to queries | You need indexes, schema or permissions per field |
| Entries should expire automatically | Records take part in graph relationships |

## Time-Series Tables (Planned — Phase 3)

A time-series table is a normal table whose RecordId is a composite array key made from
declared fields. The last key field is a `datetime`, and the fields before it identify
a **series**.

```cypher
CREATE TABLE Reading TYPE TIMESERIES KEY (sensor, ts) RETENTION 30d;
CREATE FIELD sensor ON Reading TYPE string;
CREATE FIELD ts ON Reading TYPE datetime;
CREATE FIELD value ON Reading TYPE float;
```

- `KEY (...)` lists the fields that form the key, in order. The last one must be of type
  `datetime`.
- `RETENTION` is optional. Rows whose time is older than `now - retention` are removed by
  the retention worker.
- Time-series tables are otherwise ordinary: they can have extra fields, indexes,
  permissions, events and change feeds. Records can be graph nodes.

### Writing

```cypher
CREATE (:Reading {sensor: 'sensor-1', ts: time.now(), value: 21.4});

UNWIND $batch AS row
CREATE (:Reading {sensor: row.sensor, ts: datetime(row.ts), value: row.value});
```

The engine derives the RecordId from the key fields. The record above gets an ID such
as `Reading:['sensor-1', d'2026-11-07T00:00:00Z']`. Writing the same series and time
again conflicts like any duplicate RecordId. Use `MERGE` to make ingestion idempotent.

### Range Queries

Because RecordIds are ordered by series then time, the planner turns predicates on the
key prefix into one KV range scan (see
[15 — Query Planner & Executor](15-query-planner-executor.md)):

```cypher
MATCH (r:Reading)
WHERE r.sensor = 'sensor-1'
  AND r.ts >= d'2026-11-06T00:00:00Z' AND r.ts < d'2026-11-07T00:00:00Z'
RETURN r.ts, r.value
ORDER BY r.ts;
```

`ORDER BY r.ts DESC LIMIT n` on one series is a reverse range scan, which is how you
read the latest values cheaply. A query without an equality on the leading key fields
scans every series in key order.

### Bucketing and Aggregation

`ts.bucket(datetime, duration)` returns the start of the bucket containing the
datetime. Buckets are aligned to the Unix epoch in UTC. Grouping follows normal Cypher
rules: non-aggregated return items are the grouping keys.

```cypher
MATCH (r:Reading)
WHERE r.sensor = 'sensor-1' AND r.ts >= time.now() - duration('P1D')
RETURN ts.bucket(r.ts, duration('PT1H')) AS hour,
       avg(r.value) AS avg, min(r.value) AS min, max(r.value) AS max,
       count(*) AS samples
ORDER BY hour;
```

All series at once:

```cypher
MATCH (r:Reading)
WHERE r.ts >= $from AND r.ts < $to
RETURN r.sensor, ts.bucket(r.ts, duration('PT5M')) AS bucket, avg(r.value) AS avg
ORDER BY r.sensor, bucket;
```

### Downsampling with Rollup Tables (Planned — Phase 4)

Keep raw data briefly and aggregates for longer by writing them into a second
time-series table. An event ([14 — Live Queries & Events](14-live-queries-events.md))
maintains the rollup in the same transaction as each raw write:

```cypher
CREATE TABLE ReadingHourly TYPE TIMESERIES KEY (sensor, hour) RETENTION 730d;

CREATE EVENT reading_hourly_rollup ON Reading
  WHEN $event = 'CREATE'
  THEN {
    MERGE (h:ReadingHourly {sensor: $after.sensor,
                            hour: ts.bucket($after.ts, duration('PT1H'))})
      ON CREATE SET h.count = 0, h.sum = 0.0, h.min = $after.value, h.max = $after.value
    SET h.count = h.count + 1,
        h.sum   = h.sum + $after.value,
        h.min   = CASE WHEN $after.value < h.min THEN $after.value ELSE h.min END,
        h.max   = CASE WHEN $after.value > h.max THEN $after.value ELSE h.max END
  };
```

Store `sum` and `count` instead of `avg` so rollups can be combined further (hourly into
daily). Every raw write in the same hour updates the same rollup record, so very
high-rate series can see transaction conflicts on it. For those, consume the change
feed (`db.changes`) and aggregate in batches instead.

### Retention Worker

A background worker ([16 — Engine Runtime](16-engine-runtime.md)) enforces `RETENTION`:

1. For each time-series table with a retention, compute `cutoff = now - retention`.
2. Walk the distinct series prefixes in the table.
3. For each series, delete the key range from the start of the series up to `cutoff` in
   small transactions.

Retention is eventual: rows just past the cutoff stay readable until the next pass.
Filter on time in queries that must never see older data. Deletes made by the worker
are ordinary deletes, so they appear in change feeds and fire `DELETE` events.

### Storage

Time-series records use the normal record keys from
[04 — Storage Engine](04-storage-engine.md). The composite key encoding is
memcomparable, so `['sensor-1', d'...']` sorts by sensor, then time:

```
/ns/{ns}/db/{db}/tb/Reading/r/['sensor-1', 2026-11-07T00:00:00Z]
```

## Design Decisions

**Why model time-series as tables instead of a separate engine?** Readings stay
queryable with the same Cypher, can join with graph data (sensor → device → site), and
share transactions, permissions and backup.

**Why put time in the RecordId?** Ordered keys make range scans, latest-value reads and
retention deletes contiguous, with no secondary index to maintain.

**Why events for rollups instead of a special continuous-aggregate feature?** Events are
already needed for other reactive logic. Rollups expressed as Cypher stay visible and
adjustable, and change feeds give a batch alternative when write rates are high.

**Why a separate KV keyspace instead of a table?** Pure key-value data needs no schema,
RecordIds or graph adjacency. A dedicated keyspace keeps it lean and keeps `scan` a
single range over user keys.

## What's Next

- [14 — Live Queries & Events](14-live-queries-events.md) — events, change feeds and live queries.
- [05 — Transactions & Consistency](05-transactions-consistency.md) — how KV and table writes share a transaction.
- [07 — Schema & DDL](07-schema-ddl.md) — `CREATE TABLE ... TYPE TIMESERIES` grammar.
- [24 — RAG Patterns](24-rag-patterns.md) — agent memory with TTL.
