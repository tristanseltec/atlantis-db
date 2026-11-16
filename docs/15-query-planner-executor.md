# 15 — Query Planner & Executor

> **Status:** Partially implemented — a clause-by-clause interpreter over record transactions runs every implemented Cypher feature today; the logical planner, cost-based optimizer and physical operators are Planned — Phase 2 (search operators Phase 3)
> **Crates:** `atlantis-cypher` (parser; planner and optimizer planned), `atlantis-engine` (executor)

This chapter explains how a parsed Atlantis Cypher statement is executed. It first documents the executor as it exists in the code, including its limits, then describes the target planner, optimizer and physical operators for the multi-model engine.

The query language itself is covered in [06 — Query Language](06-query-language.md).

---

## Implemented today

### Pipeline

```
Cypher script
  │  atlantis_cypher::parse_script()   lexer + parser → Vec<Statement> (AST)
  ▼
  │  Datastore::execute()              one transaction per statement, or per BEGIN … COMMIT
  ▼
  │  Exec::run()                       interprets the clauses in order against a RecordTx
  ▼
QueryResult { columns, rows }          the last statement's result
```

There is no logical plan and no optimizer. Source: `crates/atlantis-engine/src/datastore.rs`
(scripts and transactions) and `crates/atlantis-engine/src/exec/` (`mod.rs` clauses and
projection, `pattern.rs` matching and creation, `eval.rs` expressions and functions).

### Execution model

- **Rows.** A row is a map from variable to `Value`. Each clause maps the list of rows
  to a new list; the first clause starts from one empty row, so `RETURN 1` and a
  leading `UNWIND` produce rows.
- **Record ids, not copies.** Nodes and relationships are bound as `Value::RecordId`.
  Property access loads the record inside the same transaction, so a query sees its own
  earlier writes. A property holding a `RecordId` is followed the same way, so
  `o.customer.name` dereferences a stored link.
- **Matching.** Each path pattern is matched left to right from its start node. An
  unbound node pattern pinned by `id` is a direct key lookup in its labels' tables;
  otherwise it scans the label index of its first label (every non-edge table when it
  has no label) and filters by the remaining labels and inline properties. Relationship
  steps read adjacency entries in the requested direction and types. Variable-length
  steps walk depth-first with per-path edge uniqueness. Within one `MATCH`, a
  relationship is used at most once.
- **Writes.** `CREATE`, `MERGE`, `SET`, `REMOVE` and `DELETE` call `RecordTx` methods
  (`create_node`, `relate`, `update`, `merge`, `set_labels`, `delete`). `DELETE`
  collects its targets first and removes relationships before nodes.
- **Projection.** `RETURN` and `WITH` evaluate items per row, or group rows by the
  non-aggregate items when any item contains an aggregate (with `*`, the star variables
  are grouping keys too). `DISTINCT`, `ORDER BY` (on any expression, with access to the
  input variables when not aggregating), `SKIP` and `LIMIT` follow. Values are compared
  with Cypher null semantics.
- **Results.** A `RETURN` item that is a bare variable or `collect(variable)` turns
  record ids into documents with an `id` field. Columns take the alias, else the item's
  source text.
- **Procedures.** `CALL` evaluates arguments per input row and binds the yielded
  columns ([06 — Query Language](06-query-language.md#procedures)).
- **Failure.** A failing statement returns an error; its transaction is not committed,
  so none of its writes persist ([05 — Transactions & Consistency](05-transactions-consistency.md)).

### Vector search

`db.vector.search` resolves the index definition in the transaction, then searches the
in-memory HNSW graph derived from records, building it on first use. It yields the hit's
record id and a metric-aware score ([10 — Vector Index](10-vector-index.md)). Field
comparisons in the `WHERE` after `YIELD` (`node.field op value`) are pushed into the
search, which returns the `k` nearest matching records by exact or filtered-traversal
search; the whole `WHERE` then runs on the hits, so other conditions still filter
afterwards ([10 — Vector Index § Filtering](10-vector-index.md)).

### Current limits

- **Everything is materialised.** Every clause produces its full row list before the
  next starts, and `LIMIT` does not stop earlier work.
- **No row or time limits.** Unbounded variable-length patterns and large `UNWIND`s can
  use a lot of memory and time; only `range()` is capped (10 million items).
  `query.timeout` is planned ([22 — Configuration](22-configuration.md)).
- **No index selection.** A property filter (`WHERE n.email = $x`) scans the label;
  only an `id` lookup is direct. Secondary indexes arrive in Phase 2.
- **Synchronous execution.** Queries run on the calling task
  ([16 — Engine Runtime](16-engine-runtime.md)).

---

## Target design

The target pipeline separates logical planning from physical planning so that one Cypher query can be served by graph traversal, secondary indexes, vector, full-text, and spatial indexes, and joins:

```
AST ─► semantic analysis (catalog, types, permissions)
    ─► logical plan
    ─► rewrite rules (normalisation, predicate pushdown, permission injection)
    ─► cost-based optimizer (statistics, index selection, join order)
    ─► physical plan
    ─► streaming executor inside a transaction snapshot
```

### Streaming executor (Planned — Phase 2)

Operators become pull-based iterators over the transaction snapshot ([05 — Transactions & Consistency](05-transactions-consistency.md)). `LIMIT` stops upstream work early and large scans do not materialise in memory. Rows keep carrying `RecordId` values, as they do today.

### Aggregation and variable-length expand

**Aggregation (hash and streaming aggregation Phase 2).** Non-aggregated `RETURN` / `WITH` items are grouping keys, as in Neo4j and as implemented today. The target executor uses hash aggregation, and switches to streaming aggregation when input arrives sorted on the grouping keys (for example from an index scan). Relational `GROUP BY`-style patterns are expressed this way ([08 — Document & Relational](08-document-relational.md)).

**Variable-length expand.** Today's executor walks depth-first with per-path relationship uniqueness and accepts unbounded lengths. The target operator runs a breadth-first expansion over the adjacency cache or `g/` keys, bounded by `query.timeout`. `shortestPath(...)` and `allShortestPaths(...)` use bidirectional BFS (Phase 3); [09 — Graph Model](09-graph-model.md) owns their semantics.

### Statistics (Planned — Phase 2)

The optimizer keeps per-table and per-index statistics in the catalog under `/sys/...`:

- Record counts per table and per secondary label.
- Distinct-value estimates and most-common values for indexed fields.
- Average out- and in-degree per edge table.
- Index sizes for vector, full-text, and spatial indexes.

Counts are maintained incrementally at commit. Value distributions are refreshed by a background worker ([16 — Engine Runtime](16-engine-runtime.md)).

### Cost-based optimizer (Planned — Phase 2)

Logical rewrites run first:

- **Predicate pushdown** moves filters to the earliest operator that binds their variables.
- **Limit pushdown** passes `LIMIT` into sorted index scans and top-k searches.
- **Projection pruning** reads only the fields a query uses.
- **Record-link dereference** turns `o.customer.name` on a `record<Customer>` field into a key lookup instead of a join ([08 — Document & Relational](08-document-relational.md)).

The cost-based phase then enumerates access paths and join orders, estimates cardinalities from statistics, and picks the cheapest plan. Patterns with up to a small number of joins are enumerated exhaustively; larger ones fall back to a greedy order.

### Index selection (Planned — Phase 2 for B-tree, Phase 3 for search indexes)

| Predicate or call | Index | Physical operator | Status |
|---|---|---|---|
| `n.email = $x`, `n.age > 30`, `IN`, prefix `STARTS WITH` | B-tree / composite (`atlantis-index`) | `IndexSeek`, `IndexRangeScan` | Phase 2 |
| Uniqueness constraint lookup | Unique index | `IndexSeek` (at most one row) | Phase 2 |
| `ORDER BY n.field LIMIT k` | B-tree on `field` | Ordered `IndexRangeScan`, no sort | Phase 2 |
| Record key ranges, time windows on composite keys | Primary key order (`r/`) | `KeyRangeScan` | Phase 2 |
| `CALL db.vector.search(...)` with a `WHERE` on the yielded node | HNSW (`atlantis-vector`) | `VectorTopK` with pre-filter or post-filter | Phase 3 |
| `CALL db.text.search(...)` | Full-text (`atlantis-text`) | `TextSearch` (BM25) | Phase 3 |
| `CALL db.search.hybrid(...)` | Full-text + HNSW | `TextSearch` and `VectorTopK` fused with RRF | Phase 3 |
| `geo.within(p.location, $poly)`, `point.distance(...) < r` | Spatial (`atlantis-geo`) | `SpatialScan` over covering cells, then exact check | Phase 3 |

For filtered vector search, the optimizer chooses between **pre-filtering** (traverse HNSW and skip candidates that fail the filter, used when the filter is selective and indexable) and **post-filtering** (over-fetch top-k, then filter, used when the filter keeps most candidates). See [10 — Vector Index](10-vector-index.md), [11 — Full-Text Search](11-full-text-search.md), and [12 — Geospatial](12-geospatial.md).

### Join operators (Planned — Phase 2)

Graph patterns are joins on adjacency; relational patterns are joins on values. The optimizer has three operators:

| Operator | Used when |
|---|---|
| **Expand** (adjacency join) | Following edges from bound nodes through the adjacency cache or `g/` keys |
| **Index nested loop join** | The inner side has an index on the join key, or the join is a record-link dereference |
| **Hash join** | Both sides are large, or no index exists on the join key; the smaller estimated side is built |

Example: joining orders to customers by a value field without a record link.

```cypher
MATCH (o:Order), (c:Customer)
WHERE o.customerEmail = c.email AND o.total > 100
RETURN c.name, sum(o.total) AS spend
```

With an index on `Customer.email`, the plan is a filtered scan of `Order` feeding an index nested loop join into `Customer`, then hash aggregation. Without the index, it is a hash join with `Customer` as the build side.

### EXPLAIN (Planned — Phase 2)

Prefixing any query with `EXPLAIN` returns its physical plan without executing it. Each row
is one operator with its estimated rows and chosen index, so you can check that a filter was
pushed down or an index was picked:

```cypher
EXPLAIN MATCH (p:Person) WHERE p.email = $email RETURN p.name
// ProduceResults ← Project ← IndexSeek(person_email, p.email = $email) est. rows: 1
```

`EXPLAIN` needs the same `read` permissions as running the query, and the plan shows
any injected permission predicates as ordinary filters.

### Permission predicate injection (Planned — Phase 5)

Table permissions declared with `ALTER TABLE ... PERMISSIONS` ([18 — Security & Auth](18-security-auth.md)) are applied by the planner, not by the application:

- For each table a query reads, the `FOR read` predicate is added as a filter on that table's scan, with `$auth`, `$session`, `$access`, and `$token` bound from the session.
- For writes, the `FOR create`, `FOR update`, or `FOR delete` predicate is checked against each affected record before the write. A failing record is skipped for reads and rejected for writes.
- Injected predicates take part in pushdown and index selection like user predicates, so `WHERE p.author = $auth.id` can use an index on `author`.
- System users with `owner` or `editor` roles at the database level or above bypass table permissions.

### Live-query matching (Planned — Phase 4)

A `LIVE MATCH ... WHERE ... RETURN ...` query is planned once at registration:

- The planner checks that the query has a single-table root and a predicate that can be evaluated on one record. Unsupported shapes are rejected at registration time.
- The compiled predicate and projection are stored with the live query.
- For each committed change to the table, the engine evaluates the predicate on the before and after images, applies permission predicates for the subscriber's session, and emits a notification when the record enters, changes within, or leaves the result.

Dispatch is described in [16 — Engine Runtime](16-engine-runtime.md) and the user-facing semantics in [14 — Live Queries & Events](14-live-queries-events.md).

---

## Design Decisions

**Why interpret the AST directly today?**
A clause-by-clause interpreter over record transactions made the whole Cypher subset work on the record store with little machinery. It cannot stop early, bound memory or choose indexes, which is why the target adds a planner and a streaming executor.

**Why record ids in rows?**
Carrying a `RecordId` instead of a full document avoids copying records. The executor loads a record only when a property is read or the record is returned.

**Why cost-based rather than rule-based optimization?**
With B-tree, vector, full-text, and spatial indexes plus graph traversal and joins available for the same query, the right access path depends on data sizes and selectivity. Fixed rules cannot choose between, for example, pre-filtered vector search and an index seek followed by a similarity check.

**Why inject permissions in the planner?**
Permissions expressed as predicates can use indexes and pushdown, and they apply uniformly to Cypher, the HTTP key API, RPC methods, and live queries.

## What's Next

- **[06 — Query Language](06-query-language.md)** — The Atlantis Cypher syntax these plans come from.
- **[16 — Engine Runtime](16-engine-runtime.md)** — Where execution runs.
- **[09 — Graph Model](09-graph-model.md)** — Traversal, variable-length paths, and shortest paths.
- **[23 — Query Cookbook](23-query-cookbook.md)** — Practical queries.
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — Optimizer and operator phases.
