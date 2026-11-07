# 15 — Query Planner & Executor

> **Status:** Partially implemented — planner and executor with known gaps today; cost-based optimizer Planned — Phase 2
> **Crates:** `atlantis-cypher` (planner, optimizer), `atlantis-engine` (executor)

This chapter explains how a parsed Atlantis Cypher statement becomes a logical plan and how the executor turns that plan into rows. It first documents the planner and executor as they exist in the code, including their current limitations, then describes the target optimizer and physical operators for the multi-model engine.

The query language itself is covered in [06 — Query Language](06-query-language.md).

---

## Implemented today

### Pipeline

```
Cypher text
  │  atlantis_cypher::parse()      lexer + parser → Statement (AST)
  ▼
  │  atlantis_cypher::plan()       AST → LogicalOperator tree
  ▼
  │  atlantis_cypher::optimize()   pass-through (returns the plan unchanged)
  ▼
  │  QueryExecutor::execute_plan() recursive evaluation → (columns, rows)
  ▼
QueryResult { columns, rows }
```

All four steps run inside the core engine actor for each `Query` command ([16 — Engine Runtime](16-engine-runtime.md)). Source: `crates/atlantis-cypher/src/planner/` and `crates/atlantis-engine/src/executor.rs`.

### Logical operators

`LogicalOperator` (`planner/logical.rs`) defines the operators below. Not every operator is produced by the planner or executed yet.

| Operator | Planned from | Executed |
|---|---|---|
| `Empty` | Missing input | Yes, returns no rows |
| `Sequence(ops)` | `CREATE`, `SET` | Yes, returns the last operator's result |
| `NodeByLabelScan { var, label }` | Node pattern with labels (first label only) | Yes |
| `AllNodesScan { var }` | Node pattern without labels | Yes |
| `Expand { from, to, rel_type, direction }` | Relationship pattern | Partial, see limitations |
| `VarLenExpand { min, max, .. }` | Not produced | Returns no rows |
| `VectorSearch { label, query, k, threshold }` | `CALL db.vector.search(...)` | Yes |
| `Filter { predicate }` | `WHERE`, `WITH ... WHERE` | Yes |
| `Project { items }` | `RETURN`, `WITH` | Yes |
| `Sort`, `Skip`, `Limit` | `ORDER BY`, `SKIP`, `LIMIT` on `RETURN` | Yes |
| `CreateNode { var, labels, properties }` | Node patterns in `CREATE` | Yes |
| `CreateRelationship` | Not produced | Returns no rows |
| `SetProperty { target, key, value }` | `SET` | Partial, see limitations |
| `Delete { targets, detach }` | `DELETE`, `DETACH DELETE` | Partial, see limitations |
| `Unwind { expression, variable }` | `UNWIND` | Yes |

### Planning rules

`plan()` walks the clause list in order and wraps the current plan:

1. **MATCH** emits, per pattern element, a label scan or all-nodes scan for node patterns and an `Expand` for relationship patterns. `Expand` reads from the last bound node variable and binds the reached node to the fixed name `_next`.
2. **WHERE** wraps the plan in `Filter`.
3. **CREATE** emits one `CreateNode` per node pattern in a `Sequence`. Relationship patterns in `CREATE` are skipped.
4. **SET** emits one `SetProperty` per assignment in a `Sequence`.
5. **DELETE** wraps the plan in `Delete`, keeping the `DETACH` flag.
6. **CALL** supports only `db.vector.search(label, query, k[, threshold])`. The label must be a string literal and `k` an integer literal. The first two `YIELD` names become the node and score variables (defaults `node`, `score`). Any other procedure is an `InvalidQuery` error.
7. **WITH** emits `Project`, followed by `Filter` when it has a `WHERE`.
8. **UNWIND** wraps the plan in `Unwind`.
9. **RETURN** emits `Project`, then `Sort`, `Skip`, and `Limit` when present.

`MERGE`, `REMOVE`, and the `OPTIONAL` flag on `MATCH` are parsed but ignored by the planner.

Example, for `MATCH (a:Person) WHERE a.name = 'Alice' RETURN a.age AS age`:

```
Project [a.age AS age]
  └── Filter [a.name = 'Alice']
       └── NodeByLabelScan { var: "a", label: Person }
```

### Execution model

The executor materialises rows. `execute_plan` recursively evaluates an operator's input into a full `Vec<Row>`, then produces its own `Vec<Row>`. A row is a `HashMap<String, Value>`. After the root operator finishes, rows are converted to value vectors in column order, with `Null` for missing columns.

Nodes are carried in rows as `Value::Integer(node_id)`. Property access `n.prop` evaluates `n` to an ID and reads the node through `GraphStore::get_node_hot`, so nodes evicted to the cold tier are not visible ([04 — Storage Engine](04-storage-engine.md)).

Column names come from the alias, else `var` or `var.prop`, else `expr`.

### Expression evaluation

| Expression | Behaviour |
|---|---|
| Literal, parameter | Returned as is |
| Variable | Looked up in the row, then in the parameters |
| Property | Node property via the hot tier; `Null` if missing |
| `=`, `<>` | `Value` equality; `Integer` and `Float` compare numerically |
| `<`, `>`, `<=`, `>=` | `Value` ordering; incomparable types give `false` |
| `AND`, `OR` | Non-boolean operands count as `false` |
| `+`, `-`, `*`, `/` | Same-type `Integer` or `Float`; `+` also concatenates strings; integer division by zero and mixed types give `Null` |
| `IN` | Membership in an `Array` |
| `NOT`, unary `-` | Boolean negation, numeric negation |
| List literal | `Array` of evaluated items |
| `db.vector.similarity(a, b)` | Cosine similarity of two vectors |

Map literals, `CASE`, other binary operators, and other functions evaluate to nothing, which a `Filter` treats as not matching and a `Project` returns as `Null`.

### Vector search

`VectorSearch` evaluates the query to a `Value::Vector`, calls `HnswIndex::search` (or `search_with_params` when a threshold is given), and emits rows with the node ID and `score = 1.0 - distance`. The label argument is not applied as a filter, and the engine does not insert vectors into the index today ([10 — Vector Index](10-vector-index.md), [16 — Engine Runtime](16-engine-runtime.md)).

### Current limitations

These are Phase 0 work in [25 — Implementation Roadmap](25-implementation-roadmap.md):

- **Multi-hop patterns.** The node reached by `Expand` is bound to `_next`, and the following node pattern emits a new scan whose `input` the executor ignores. In `MATCH (a)-[:KNOWS]->(b:Person)`, `b` is therefore bound to every `Person`, not to `a`'s neighbors. Single-node patterns and scans behave correctly.
- **Expand direction and type.** `Expand` follows outgoing edges only and ignores the declared direction. A relationship pattern without a type produces no rows.
- **SET after MATCH.** The planner builds each `SetProperty` on an empty input instead of the matched rows, so `MATCH ... SET ...` updates nothing.
- **DETACH DELETE.** Edges are not removed; the node is tombstoned and filtered from traversal.
- **CREATE.** Relationships are not created, and a multi-node `CREATE` returns only the last created node.
- **Queries without a leading MATCH or CREATE.** `UNWIND` and `RETURN` as the first clause run over an empty input and return no rows, so `RETURN 1` and `UNWIND [1, 2] AS x RETURN x` produce nothing.
- **Not implemented.** Variable-length expansion, `MERGE`, `OPTIONAL MATCH`, aggregation functions, and `count(*)`.
- **Sort.** A descending key returns its comparison immediately, so later sort keys are not consulted for descending sorts.
- **Optimizer.** `optimize()` returns its input unchanged.

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

### Streaming executor (Planned — Phase 1)

Operators become pull-based iterators over a transaction snapshot ([05 — Transactions & Consistency](05-transactions-consistency.md)). `LIMIT` stops upstream work early, large scans do not materialise in memory, and every read sees the same snapshot. Rows carry `RecordId` values instead of raw integer IDs (Phase 2, [03 — Data Model](03-data-model.md)).

### Phase 0 operator completion

| Capability | Status |
|---|---|
| Correct multi-hop binding: `Expand` binds the pattern's own target variable and later node patterns become label filters on it | Phase 0 |
| `Expand` honours `OUT`, `IN`, and `BOTH`, and untyped relationships | Phase 0 (`IN`/`BOTH` need `in` adjacency, Phase 1) |
| `CreateRelationship` and `DETACH DELETE` | Phase 0 |
| `MERGE` as match-or-create | Phase 0 |
| `OPTIONAL MATCH` as a left outer apply | Phase 0 |
| Aggregation: `count`, `sum`, `avg`, `min`, `max`, `collect` with implicit grouping keys | Phase 0 |
| `VarLenExpand` | Phase 0 |

**Aggregation (Planned — Phase 0, hash aggregation Phase 2).** Non-aggregated `RETURN` / `WITH` items become grouping keys, as in Neo4j. The executor uses hash aggregation; when input arrives sorted on the grouping keys (for example from an index scan), it switches to streaming aggregation. Relational `GROUP BY`-style patterns are expressed this way ([08 — Document & Relational](08-document-relational.md)).

**Variable-length expand (Planned — Phase 0).** `MATCH (a)-[:KNOWS*1..3]->(b)` runs a breadth-first expansion with per-path relationship uniqueness, as in Cypher. Each hop reads the adjacency cache or `g/` keys. An upper bound is required unless the query uses `shortestPath(...)` or `allShortestPaths(...)`, which use bidirectional BFS; [09 — Graph Model](09-graph-model.md) owns their semantics and phase.

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

**Why materialised rows today?**
A recursive evaluator over `Vec<Row>` was the fastest way to make queries work end to end. It cannot stop early or bound memory, which is why the target executor streams.

**Why node IDs in rows?**
Carrying a `u64` instead of a full node avoids cloning property maps. The executor fetches a node only when a property is accessed. The target keeps this idea with `RecordId` values.

**Why cost-based rather than rule-based optimization?**
With B-tree, vector, full-text, and spatial indexes plus graph traversal and joins available for the same query, the right access path depends on data sizes and selectivity. Fixed rules cannot choose between, for example, pre-filtered vector search and an index seek followed by a similarity check.

**Why inject permissions in the planner?**
Permissions expressed as predicates can use indexes and pushdown, and they apply uniformly to Cypher, the HTTP key API, RPC methods, and live queries.

## What's Next

- **[06 — Query Language](06-query-language.md)** — The Atlantis Cypher syntax these plans come from.
- **[16 — Engine Runtime](16-engine-runtime.md)** — Where planning and execution run.
- **[09 — Graph Model](09-graph-model.md)** — Traversal, variable-length paths, and shortest paths.
- **[23 — Query Cookbook](23-query-cookbook.md)** — Practical queries.
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — Phase 0 executor work and later optimizer phases.
