# 29 — Core Engine Design

> **Status:** Target design. Reviewed 2026-11-17 against `main` at `48ec88c`; every "today" statement below was checked in the code or measured. Nothing in the *Design* section is implemented unless it says so.
> **Crates:** `atlantis-core`, `atlantis-kv`, `atlantis-storage`, `atlantis-cypher`, `atlantis-engine`, `atlantis-db`

The goal in one sentence: **store documents as freely as MongoDB, answer queries with
the speed and predictability of PostgreSQL, and let people think in nodes and
relationships as they do in Neo4j**, all on one record, one transaction and one query
language.

This chapter states what each of those three promises means in measurable terms, where
they pull against each other, and the engine design that reconciles them. It ends with
ordered milestones that collaborators can pick up.

## The Three Promises

| Promise | What a user gets | How we measure it | Today |
|---|---|---|---|
| **Flexible like MongoDB** | Write any JSON-shaped document to any label without DDL; nest objects and arrays; add fields at any time; add schema later, one field at a time, without migrations | Every cookbook recipe runs without DDL; a JSON corpus round-trips exactly | Schemaless records and nested documents work. No JSON import or export, field order is lost, no document API ([28](28-document-model.md)) |
| **Fast like PostgreSQL** | Microsecond indexed lookups, scans at tens of nanoseconds per row, joins chosen by cost, compact storage, data larger than memory, plans that do not surprise | Microbenchmarks below, a TPC-H subset, YCSB | Indexes and `EXPLAIN` work (PR #8). Scans cost about 4 µs per row, the interpreter about 6x a raw lookup, every property read decodes the whole record, memory only |
| **Graph mindset like Neo4j** | Relationships are first-class records with properties; Cypher patterns; the cost of a hop depends on the node's degree, not the graph's size; variable-length paths | LDBC SNB Interactive short and complex reads | Cypher patterns and edge records work. Each hop reads every edge record to find its neighbour; adding an edge rewrites both endpoint documents |

### What we deliberately do not copy

- **From MongoDB:** storing every field name in every document, and the absence of
  joins and multi-document transactions. Atlantis keeps ACID transactions and joins.
- **From PostgreSQL:** a mandatory schema, migrations before writes, and SQL. Cypher
  stays the one query language ([26 — Positioning](26-positioning-and-targets.md)).
- **From Neo4j:** the pointer-based store with fixed-size records. It is fast for hops
  but cannot sit on an ordered, MVCC, eventually distributed key-value layer. Atlantis
  gets locality from clustered keys and a derived in-memory cache instead.

### Why this is not a breadth play

[26 — Positioning](26-positioning-and-targets.md) says Atlantis does not win on
breadth. These three are not three products; they are qualities of one engine that the
focus workload, agent memory and GraphRAG, already needs. Memory payloads are
irregular documents, retrieval is filtered lookups and joins, and context is graph
expansion. The milestones are ordered so the first ones make the Retrieval Bench
faster.

## Where the Promises Conflict

| Conflict | Naive outcome | Resolution in this design |
|---|---|---|
| Flexible documents vs compact, fast storage | Self-describing documents (MongoDB, JSONB): every record repeats its field names and must be parsed to read one field | **Shapes**: records with the same fields share a catalog entry and store values by position (§2) |
| Graph locality vs an ordered MVCC key-value store | Each hop is a set of random reads | **Clustered adjacency** carrying the neighbour id, plus a **derived adjacency cache** for multi-hop work (§4) |
| Free-form queries vs predictable plans | Without types or statistics, a planner guesses | Statistics from shapes, indexes and adjacency; a **cost-based planner**; and **gradual schema**, which lets users promise types where they matter (§1, §3) |

## Design

```
            Cypher                           Document API (Rust, then Python)
              │   standard Cypher semantics     │   MongoDB matching semantics
              └───────────────┬─────────────────┘
                     logical plan ─► cost-based optimizer ─► physical plan       §3
                              │        statistics ◄── shapes, indexes, degrees
                 streaming batched executor, slots, plan cache                    §3
                              │
       records (format v2: shapes)  ·  property indexes  ·  clustered adjacency   §2 §4
                              │                                    │
                 MVCC key-value layer (mem today, RocksDB next)   adjacency cache   §4 §6
```

### 1. Gradual schema: the flexibility ladder

A label can sit on any rung, and moving between rungs never rewrites stored data. Moving
up only validates what is there.

| Rung | The user does | The engine does | Status |
|---|---|---|---|
| **Schemaless** | Nothing | Accepts any document; records shapes (§2) | Implemented (shapes planned) |
| **Observed** | Nothing | Keeps per-field statistics: presence, type mix, distinct values. `SHOW FIELDS ON Person` reports them | Planned |
| **Constrained** | `CREATE CONSTRAINT ... IS UNIQUE / NOT NULL / NODE KEY`, `CREATE INDEX` | Enforces on every write; indexes serve lookups | Implemented |
| **Typed fields** | `CREATE FIELD email ON Person TYPE string ASSERT ...` | Rejects writes that break a declared field; other fields stay free | Planned ([07](07-schema-ddl.md)) |
| **Strict** | `CREATE TABLE Person SCHEMAFULL` | Rejects undeclared fields | Planned ([07](07-schema-ddl.md)) |

Rules:

- Declaring a field or constraint checks existing records first and fails with the
  first offending record. This is how constraint creation already behaves.
- The planner uses whatever the label promises. A typed field gets exact statistics and
  no type checks in filters; an untyped field gets the observed type mix.
- Query semantics do not depend on the rung. Cypher keeps standard semantics; the
  document API keeps MongoDB's ([28 — Decision](28-document-model.md#decision-two-front-ends-one-engine)).

### 2. Record format v2: shapes

**Today (verified).** A record is a bincode-encoded `BTreeMap<String, Value>`. Every
record stores every field name; field order is lost; reading `p.email` decodes the
whole document, and `RETURN p.a, p.b, p.c` decodes it three times
(`exec/eval/access.rs`).

**Design.** A *shape* is the ordered list of top-level field names of a record, with a
type tag per field. Records of the same shape share one catalog entry and store only
their values, by position, like a PostgreSQL heap tuple:

```
format byte 0x02 │ shape id │ null/missing bitmap │ slot offsets │ slot values … │ overflow map (rare fields)
```

- **Shape ids are content hashes** of the field list. Two transactions that meet the
  same new shape write the same catalog entry, `/sys/.../sh/{table}/{id}`, with
  `set_idempotent`, so they never conflict. A hash collision with a different field
  list takes the next id.
- **Insertion order is the shape's field order.** This fixes the lost field order
  ([28](28-document-model.md), P0).
- **Lazy reads.** A `RecordView` decodes the header and only the slots a query
  touches. Combined with §3's single decode per statement, this removes the repeated
  full decodes.
- **Nested objects** get shapes too, up to a fixed depth; arrays stay self-describing.
- **Rare fields** go into a per-record overflow map instead of minting a new shape.
  A table that exceeds a shape budget (for example 1,024) writes new combinations in
  the self-describing form. Heterogeneous data degrades gracefully instead of filling
  the catalog.
- **Large values** (long strings, bytes) move out of line to
  `/tb/{tb}/lv/{key}/{field}`, as PostgreSQL's TOAST does, so scans that do not read
  them stay small. Vectors already live in their own index.
- **Shapes are the observed schema.** Per-shape counts give field presence and type
  mix for free (§1, §3).
- **Migration is lazy.** Version 1 records keep decoding; a record is rewritten in
  version 2 on its next update. There is no offline migration.

**Expected effect, to be measured before it is claimed:** smaller records for typical
10–20 field documents, constant-time access to one field, and cheaper scans.

### 3. Execution: from interpreter to engine

**Today (verified).** A row is a `HashMap<String, Value>`; every clause materialises
all rows before the next starts; `LIMIT` does not stop earlier work; each statement is
parsed and planned on every call. Index selection is rule-based (PR #8). Baseline: a
Cypher lookup by id costs 9.3 µs against 1.5 µs for the raw read, and scan, filter and
aggregate over 10k rows takes 39.8 ms ([26 § Baseline](26-positioning-and-targets.md#baseline-2026-11-16)).

**Design.**

- **Slots, not maps.** Variables compile to slot indices at plan time; a row is a small
  vector. Node slots hold the record id and, once read, the record view, so each record
  is decoded once per statement.
- **Streaming, batched pull execution.** Operators exchange batches of about 1,024
  rows. `LIMIT` stops upstream work, and memory stays bounded.
- **Plan cache.** Plans are cached by normalised query text and parameter types, and
  invalidated by the schema version that PR #8 introduced. Repeated agent queries skip
  parsing and planning.
- **Statistics.**
  - Per label: record counts, maintained at commit.
  - Per field: presence and type mix from shapes, plus distinct-value counts and
    histograms from sampling (`ANALYZE`, and during index builds).
  - Per relationship type and direction: average and high-percentile degree.
- **Cost-based planning.**
  - Join order by dynamic programming for small patterns, greedy for large ones.
  - Which end of a path to start from, and the direction of each expand.
  - Index seek or scan; index nested loop or hash join.
  - Index-ordered scans for `ORDER BY … LIMIT`.
  - Covering indexes (`INCLUDE` fields) that answer without reading the record.
  - Today's rule-based choice remains the fallback when statistics are missing.
- **`EXPLAIN` gains estimates**, and `PROFILE` gains actual rows and time per operator.

### 4. Graph storage

**Today (verified).**

- An adjacency key is `/tb/{tb}/g/{node}/{dir}/{edgeTable}{edgeKey}` with an empty
  value. Expanding a node with *d* edges costs one prefix scan, *d* edge-record reads to
  learn each neighbour, and *d* neighbour reads.
- `relate` rewrites both endpoint records unchanged so that a concurrent delete
  conflicts with it. Two concurrent edge inserts on one node therefore always conflict,
  and each new edge rewrites the node's whole document, which is costly for
  high-degree nodes.

**Design.**

- **Adjacency v2.** The value of an adjacency entry holds the neighbour's id and,
  optionally, a small inline copy of the edge's properties: fields marked `INLINE`, or
  edges under a size limit. Expand and type, direction and inline-property filters then
  read one key range and never touch edge records. The edge record stays the source of
  truth, and both entries are maintained in the same transaction.
- **Edges without rewriting endpoints.** `relate` *watches* each endpoint's record key
  instead of rewriting it, using the `KvTransaction::watch` primitive from PR #8.
  `delete` watches the node's adjacency range. A delete and a concurrent `relate`
  still conflict; two concurrent `relate`s on the same node no longer do.
- **Degree statistics** per (label, type, direction), not exact per-node counters,
  which would turn every high-degree node into a hot key. The planner uses them to
  detect supernodes and order expands.
- **Adjacency cache.** This is [04 § Adjacency cache](04-storage-engine.md#adjacency-cache-planned--phase-1)
  with a concrete layout: per edge table and direction, a compressed sparse row (CSR)
  array over dense node ordinals, with versioned deltas from commits. Readers whose
  snapshot it cannot serve, or with their own uncommitted edges, fall back to key
  scans. Multi-hop and variable-length patterns run on it, which gives Neo4j-like hop
  cost without giving up MVCC.
- **Relationship property indexes**: `CREATE INDEX FOR ()-[r:T]-() ON (r.p)`, the
  remainder of [07](07-schema-ddl.md).

### 5. Document surface

JSON import and export, update operators, the document API and its MongoDB filter
semantics are specified in [28 — Document Model](28-document-model.md) (work packages
1, 2, 3 and 6). Shapes (§2) are what make that flexibility cheap.

### 6. Storage backend

The RocksDB backend (Phase 1, kept in [26 § Scope](26-positioning-and-targets.md#scope-decisions))
brings larger-than-memory data:

- The block cache plays the role of PostgreSQL's buffer pool.
- Prefix bloom filters on table, node and index prefixes keep point and adjacency reads
  to one disk lookup.
- Format v2 records compress well because they hold no field names.

The key layout needs no change.

## Targets

Targets are measured in the benchmark harness on the same machine as the baseline.
Following the README, no claim is published before it is measured. Comparisons
against server databases (PostgreSQL, MongoDB, Neo4j) run them on the same machine over
loopback and state that Atlantis is embedded.

| Metric | Today | Target | Compared with |
|---|---|---|---|
| Cypher lookup by id | 9.3 µs | ≤ 3 µs | Raw read: 1.5 µs |
| Cypher lookup on an indexed field | 24 µs (PR #8) | ≤ 5 µs | PostgreSQL prepared statement, MongoDB `findOne` |
| Scan, filter and aggregate, 10k rows | 39.8 ms | < 1 ms (already in 26) | PostgreSQL |
| Two-hop traversal | 23.8 µs | ≤ 10 µs | Neo4j |
| Concurrent `relate` on one node, 8 writers | Conflicts | No conflicts | — |
| Bulk ingest with a unique constraint and an index, in memory | 62k/s | ≥ 100k/s | MongoDB |
| Size of a typical 15-field document | To measure | ≥ 30% below format v1 | MongoDB BSON |
| Suites | — | LDBC SNB Interactive SF1, YCSB A–C, TPC-H subset SF1 (Phase 2 exit criterion), Retrieval Bench | Neo4j, MongoDB, PostgreSQL |

## Milestones

Each milestone is a set of pull requests with an exit criterion. M1 and M2 make the
Retrieval Bench faster, so they can run alongside the focus track (F4, F5). Later
milestones follow it. This ordering is a recommendation.

| # | Milestone | Scope | Exit criterion |
|---|---|---|---|
| M0 | **Measure first** | Add YCSB-style document, LDBC SNB short-read and TPC-H-subset workloads and record size to the harness; record baselines | Baselines published in [26](26-positioning-and-targets.md) |
| M1 | **Executor foundations** | Slots, single decode per record per statement, streaming batches, `LIMIT` early exit, plan cache | Lookup by id ≤ 3 µs; scan, filter and aggregate over 10k rows ≤ 5 ms |
| M2 | **Graph write and expand path** | `relate` and `delete` with watches; adjacency v2 with neighbour ids and inline properties | Zero conflicts for 8 writers on one node; an expand reads one key range |
| M3 | **Record format v2** | Shapes, lazy views, insertion order, out-of-line large values, `SHOW SHAPES` | Size and field-access targets met; v1 data reads and upgrades on write |
| M4 | **Cost-based planner** | Statistics, `ANALYZE`, join order, start side, hash join, index-ordered scans, covering indexes, `PROFILE` | TPC-H subset runs; LDBC short reads start from the selective side |
| M5 | **Adjacency cache** | CSR per edge table and direction with versioned deltas | LDBC complex reads at target; two-hop ≤ 10 µs |
| M6 | **Gradual schema** | `CREATE FIELD`, types, `ASSERT`, `DEFAULT`, `SCHEMAFULL`, `Decimal`, observed field statistics | Schemafull tables reject invalid writes; `SHOW FIELDS` reports observed types |
| M7 | **Document surface** | [28](28-document-model.md) packages 1, 2, 3 and 6 | JSON corpus round-trips; MongoDB filter conformance suite passes |
| M8 | **Larger than memory** | RocksDB backend with prefix bloom filters | Retrieval Bench `large` scale runs from disk |

M7 is independent of M1–M5 and suits contributors who want to work on the API rather
than the engine core.

## Risks and Trade-offs

- **Shape explosion.** Truly irregular data creates many shapes. The overflow map and
  the per-table shape budget cap the cost, and `SHOW SHAPES` makes it visible.
- **Inline edge properties cost writes.** Each inline field is written three times
  (edge record and two adjacency entries), so inlining stays opt-in or size-limited.
- **Cost models misjudge skewed graphs.** Degree statistics keep high percentiles, and
  rule-based planning remains the fallback; plan hints may be needed later.
- **Plan cache staleness.** Plans are keyed by schema version, so DDL invalidates them;
  statistics drift is handled by re-planning when estimates are badly off.
- **Not an analytics engine.** Row-oriented records will not match columnar engines
  (DuckDB, ClickHouse) on large aggregations. That is out of scope.
- **Embedded comparisons flatter us.** Embedded calls skip the network. Benchmarks say
  so, and also compare with embedded engines where one exists.
- **Breadth.** Every milestone must pay off on the focus workload or close a measured
  gap; otherwise it waits.

## Design Decisions

**Shapes over columns and over self-describing documents.** Shapes keep row-at-a-time
writes and transactions cheap, which agent memory needs, while removing most of the
cost of flexibility. Columnar layouts would favour analytics over the workload we
target.

**Clustered keys plus a derived cache over a pointer store.** Ordered keys survive
MVCC, RocksDB and a future cluster unchanged; the cache gives hop speed where it
matters and is never the source of truth.

**Watches instead of rewrites for conflict safety.** PR #8 introduced
`KvTransaction::watch` for schema changes; the same primitive removes the endpoint
rewrite in `relate`, the main obstacle to concurrent graph ingest.

**Statistics before a cost-based optimizer.** A cost model without statistics is
guesswork. M4 builds statistics first and keeps rule-based selection as the fallback.

**One semantics per front end.** Cypher stays standard; MongoDB semantics live only in
the document API ([28](28-document-model.md)).

## What's Next

- **[28 — Document Model](28-document-model.md)**: the document surface (M7).
- **[15 — Query Planner & Executor](15-query-planner-executor.md)**: today's planner and the target pipeline (M1, M4).
- **[04 — Storage Engine](04-storage-engine.md)**: key layout and the adjacency cache (M2, M5, M8).
- **[26 — Positioning & Performance Targets](26-positioning-and-targets.md)**: focus track and baselines (M0).
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)**: phases.
