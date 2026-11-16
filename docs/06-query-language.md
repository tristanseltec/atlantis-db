# 06 — Atlantis Cypher Query Language

> **Status:** Partially implemented — core Cypher reads and writes, scripts with `BEGIN`/`COMMIT`, and vector index DDL work today on the record store; the other dialect extensions are Planned — Phases 2–7
> **Crates:** `atlantis-cypher`, `atlantis-engine`

Atlantis DB has one query language: **Atlantis Cypher**. It is Cypher as defined by
Neo4j 5, openCypher, and GQL, plus extensions for the models Cypher has no syntax for
(schema, documents, search, key-value, time-series, live queries, security). Where
Cypher already has a form, Atlantis Cypher uses it unchanged.

This chapter covers two things:

1. **Implemented today** — the subset the lexer, parser and executor accept now,
   including what is missing, so you can write queries that actually work.
2. **Target design** — the full extension catalogue, with the chapter that owns each
   construct and the roadmap phase that delivers it.

## Implemented Today

The behaviour below is verified against the source and exercised by the integration
tests in `crates/atlantis-db/tests/cypher.rs`, which are the authoritative list of
working behaviour.

### Pipeline

```
script text ─► tokenize ─► parse_script ─► statements ─► interpret clause by clause ─► QueryResult
               lexer/       parser/                      atlantis-engine exec/
```

`atlantis_cypher::parse` parses one statement and `atlantis_cypher::parse_script` a
`;`-separated script (`crates/atlantis-cypher/src/lib.rs`). There is no planner or
optimizer: `atlantis-engine` interprets the AST clause by clause against a record
transaction. See [15 — Query Planner & Executor](15-query-planner-executor.md).

### Scripts and transactions

A script is a sequence of statements separated by `;`; empty statements are ignored.
Each statement outside `BEGIN; ... COMMIT;` runs and commits in its own transaction.
Statements between `BEGIN` and `COMMIT` share one transaction; `ROLLBACK` discards it,
and a script that ends inside an open `BEGIN` is rolled back with an error.
`Engine::execute` returns the result of the last statement. Inside a Rust transaction,
`Transaction::execute` runs statements in that transaction and rejects
`BEGIN`/`COMMIT`/`ROLLBACK`. Details are in
[05 — Transactions & Consistency](05-transactions-consistency.md).

### Clauses

| Clause | Status | Notes |
|---|---|---|
| `MATCH`, comma-separated patterns, several `MATCH` clauses | Implemented | A node pattern scans the label index of its labels (or every non-edge table when it has none), or looks a record up directly when it pins `id` (trying each of its labels' tables, so secondary labels work too). Inline properties filter. Relationships never match a node pattern. Within one `MATCH`, a relationship is used at most once. |
| Relationship patterns `-[r:T]->`, `<-[r:T]-`, `-[r:T]-`, `-->`, `--`, `:A\|B` | Implemented | Any direction, untyped or with alternative types, with inline properties. |
| Variable length `*`, `*n`, `*min..max`, `*..max`, `*min..` | Implemented | A missing minimum is 1. Unbounded lengths walk until edges run out; each edge is used once per path. |
| Path variables `p = (...)` | Implemented | A path is the map `{nodes: [...], relationships: [...]}` of record ids. |
| `OPTIONAL MATCH` | Implemented | Unmatched rows keep their input with the new variables set to `null`. |
| `WHERE` | Implemented | Rows pass only when the predicate is exactly `true`. |
| `CREATE` | Implemented | Nodes, relationships (exactly one type and a direction), and paths; bound variables are reused. Runs once per input row and returns every row. |
| `MERGE ... [ON CREATE SET ...] [ON MATCH SET ...]` | Implemented | One path pattern. Matches the whole path, or creates it. A `null` property value in the pattern is an error. |
| `SET` | Implemented | `n.a.b = v` (nested; `null` removes), `n = {map}` (replace), `n += {map}` (merge), `n:Label` (secondary labels). |
| `REMOVE` | Implemented | `n.a.b` and `n:Label`. A node's first label is its table and cannot be removed. |
| `DELETE`, `DETACH DELETE` | Implemented | All targets of the clause are collected first; relationships are deleted before nodes, so `DELETE n, r` works in either order. A node that still has other relationships fails with `SchemaViolation` unless `DETACH DELETE` is used. A path value contributes its relationships and nodes. |
| `WITH ... [WHERE ...]` | Implemented | Projection, aggregation, `DISTINCT`, `ORDER BY`, `SKIP`, `LIMIT`. |
| `UNWIND list AS x` | Implemented | Also as the first clause. `null` gives no rows; a non-list gives one row. |
| `RETURN` | Implemented | `*` (with aggregates, the star variables are grouping keys), `AS`, `DISTINCT`, `ORDER BY` any expression with `ASC`/`DESC`, `SKIP` and `LIMIT` in either order. Also valid as the first clause (`RETURN 1 AS x`). Only `UNION`, `;` or the end of input may follow; anything else is a parse error. |
| `UNION`, `UNION ALL` | Implemented | All parts must return the same columns. |
| `CALL proc(args) [YIELD col [AS alias], ...] [WHERE ...]` | Implemented | Procedures below. A query ending in `CALL` returns the yielded columns. |
| `CREATE VECTOR INDEX`, `DROP INDEX` | Implemented | See [10 — Vector Index](10-vector-index.md). |
| `BEGIN`, `COMMIT`, `ROLLBACK` | Implemented | Script-level; see above. |
| `shortestPath`, `allShortestPaths` | Phase 3 | See [09 — Graph Model](09-graph-model.md). |
| Map projection `n {.*, .name}`, `EXISTS { ... }` and `CALL { ... }` subqueries, `FOREACH`, `LOAD CSV` | Not implemented | Parse errors today; not scheduled in a phase yet. |

### Expressions

| Construct | Status |
|---|---|
| Literals: integer, float, single- or double-quoted string, `true`, `false`, `null`, lists, maps | Implemented. Maps are `Object` values. |
| Parameters `$name` | Implemented. A missing parameter is an error, and so is an unbound variable. |
| Property access `n.a.b`, `map.key`, `list[i]`, `list[a..b]`, `map['key']` | Implemented. On a node or relationship it reads the record in the same transaction; on a stored `RecordId` it follows the link (`o.customer.name`). |
| `=`, `<>`, `!=`, `<`, `<=`, `>`, `>=` | Implemented, with Cypher null rules, also inside lists and maps: `[1, null] = [1, null]` is `null`. `DISTINCT` follows the same rules. |
| `AND`, `OR`, `XOR`, `NOT` | Implemented, with three-valued logic |
| `IN`, `IS [NOT] NULL`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `=~` (full-match regex) | Implemented |
| `+`, `-`, `*`, `/`, `%`, `^` | Implemented; `+` also concatenates strings and lists |
| Label predicate `n:Label` | Implemented |
| `CASE` (simple and searched) | Implemented |
| List comprehension `[x IN list WHERE p \| expr]` | Implemented |
| Aggregates `count`, `count(*)`, `sum`, `avg`, `min`, `max`, `collect`, `stdev`, with `DISTINCT` | Implemented, with implicit grouping by the non-aggregate items |
| Scalar functions | Implemented; the list is `call_function` in `crates/atlantis-engine/src/exec/eval.rs` |

Function names are case-insensitive and may be dotted (`db.vector.similarity`). Today's
functions cover identity and structure (`id`, `elementId`, `rid`, `labels`, `type`,
`startNode`, `endNode`, `properties`, `keys`, `nodes`, `relationships`, `length`),
lists and strings, conversions, math, `coalesce` and `exists(expr)`, temporal values
(`datetime`, `duration`, `timestamp`, `time.now`), points (`point`, `point.distance`)
and `db.vector.similarity`.

### Procedures

| Procedure | Yields |
|---|---|
| `db.vector.search(indexOrLabel, vector, k[, threshold \| {threshold, efSearch}])` | `node`, `score` |
| `db.labels()` | `label` (node tables) |
| `db.relationshipTypes()` | `relationshipType` (edge tables) |
| `db.indexes()` | `name`, `type`, `label`, `property`, `dimension`, `metric` |

### Results

`QueryResult` holds `columns` and `rows`. A column takes its alias, else the source text
of the expression. Nodes and relationships are bound to their `RecordId`; a `RETURN`
item that is a bare variable (or `collect(variable)`) returns the record's document,
with an `id` field holding the `RecordId` (relationship documents also carry `in` and
`out`). `n.id` and `id(n)` return the `RecordId` itself. Typed accessors live in
`ResultSet`; see [21 — API Reference](21-api-reference.md).

### Lexer and parser

**Files:** `crates/atlantis-cypher/src/lexer/`, `crates/atlantis-cypher/src/parser/`

- **Keywords** are case-insensitive; identifiers, labels and property keys keep their
  case. Backticks quote a name (`` `my var` ``); table names (first labels and
  relationship types) must still be valid identifiers.
- **Strings** use single or double quotes with the usual backslash escapes.
- **Numbers**: `1..3` lexes as `1`, `..`, `3`, so ranges and `*1..3` work.
- **Comments**: `// line` and `/* block */`.
- **Errors** are `AtlantisError::Parse` with a `line:column` position.

The parser is hand-written recursive descent producing the AST in
`crates/atlantis-cypher/src/parser/ast.rs`; read it for the exact variant list rather
than relying on a copy here. Expression precedence, lowest first: `OR`, `XOR`, `AND`,
`NOT`, comparison, `+ -`, `* / %`, `^`, unary `- +`, postfix (`.key`, `[i]`, `:Label`),
atoms.

### Working Examples

```cypher
MATCH (a:Person {name: $name})-[:KNOWS*1..2]->(f:Person)
RETURN DISTINCT f.name AS friend ORDER BY friend
```

```cypher
MATCH (p:Person) OPTIONAL MATCH (p)-[:WORKS_AT]->(c:Company)
RETURN p.name AS name, c.name AS company
```

```cypher
MERGE (p:Person {email: 'a@x.io'})
ON CREATE SET p.created = true, p.visits = 1
ON MATCH SET p.visits = p.visits + 1
```

```cypher
MATCH (s:Sale)
RETURN s.region AS region, sum(s.amount) AS total, count(*) AS n
ORDER BY total DESC SKIP 1 LIMIT 2
```

```cypher
CALL db.vector.search('chunk_embedding', $query_vec, 10) YIELD node, score
RETURN node.text AS text, score
```

## Target Design: Atlantis Cypher (Planned)

### Language Principles

- **Cypher first.** Any construct that exists in Neo4j 5, openCypher, or GQL keeps that
  syntax and meaning: `MERGE ... ON CREATE SET`, `OPTIONAL MATCH`, `UNION`, `CASE`,
  aggregation with implicit grouping, list comprehensions and path variables (all
  implemented), and map projections, `shortestPath`, `CREATE INDEX ... FOR ... ON`,
  `CREATE CONSTRAINT ... REQUIRE`, `SHOW INDEXES` (planned).
- **Extensions only where Cypher has nothing.** New statements use Cypher's
  keyword-first shape (`CREATE TABLE`, `CREATE FIELD`, `LIVE MATCH`). New behaviour that
  fits a procedure or function uses a dotted namespace (`db.text.search`, `geo.within`)
  and the `CALL ... YIELD` convention.
- **One record model.** Every node, edge, document row, key-value entry, and
  time-series point is a record with a `RecordId`. See [03 — Data Model](03-data-model.md).

### Extension Catalogue

The owning chapter holds the full grammar and semantics. The phase refers to
[25 — Implementation Roadmap](25-implementation-roadmap.md).

#### Statements

| Construct | Example | Owner | Status |
|---|---|---|---|
| Core Cypher (`MERGE`, `SET`, `REMOVE`, `OPTIONAL MATCH`, `UNION`, `CASE`, `IN`, `IS NULL`, string predicates, `<>`, incoming and undirected patterns, comma patterns) | `MERGE (p:Person {email: $e}) ON CREATE SET p.created = time.now()` | this chapter, [08](08-document-relational.md), [09](09-graph-model.md) | Implemented |
| Relationship `CREATE`, `DETACH DELETE`, variable-length paths | `MATCH (a)-[:KNOWS*1..3]->(b)` | [09](09-graph-model.md) | Implemented |
| Aggregation and grouping | `RETURN c.name, count(o) AS orders` | [08](08-document-relational.md) | Implemented |
| Path variables and path functions | `MATCH p = (a)-[:KNOWS*1..3]->(b) RETURN length(p)` | [09](09-graph-model.md) | Implemented |
| `shortestPath`, `allShortestPaths` | `MATCH p = shortestPath((a)-[:KNOWS*]-(b))` | [09](09-graph-model.md) | Phase 3 |
| Multi-statement scripts, `BEGIN` / `COMMIT` / `ROLLBACK` | `BEGIN; ...; COMMIT;` | [05](05-transactions-consistency.md) | Implemented |
| `CREATE NAMESPACE`, `CREATE DATABASE`, `USE` | `USE acme.shop;` | [07](07-schema-ddl.md) | Phase 2 |
| `CREATE TABLE ... SCHEMAFULL / SCHEMALESS`, `TYPE EDGE` | `CREATE TABLE WORKS_AT TYPE EDGE FROM Person TO Company;` | [07](07-schema-ddl.md) | Phase 2 |
| `CREATE FIELD` with `TYPE`, `DEFAULT`, `ASSERT`, `COMPUTED`, `READONLY` | `CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';` | [07](07-schema-ddl.md) | Phase 2 |
| `ALTER TABLE`, `DROP TABLE`, `SHOW ...` introspection | `SHOW TABLES` | [07](07-schema-ddl.md) | Phase 2 |
| `CREATE INDEX`, `CREATE CONSTRAINT` (Neo4j 5 forms) | `CREATE INDEX person_email FOR (p:Person) ON (p.email);` | [07](07-schema-ddl.md), [08](08-document-relational.md) | Phase 2 |
| Nested document paths in `SET` and `REMOVE` | `SET p.address.city = 'Hue'` | [08](08-document-relational.md) | Implemented |
| `CREATE TABLE ... TYPE TIMESERIES` | `CREATE TABLE Reading TYPE TIMESERIES KEY (sensor, ts) RETENTION 30d;` | [13](13-key-value-time-series.md) | Phase 3 |
| `CREATE VECTOR INDEX`, `DROP INDEX` | `... OPTIONS {dimension: 1536, metric: 'cosine'}` | [10](10-vector-index.md) | Implemented (filtered search via `YIELD ... WHERE`; quantization Phase 3) |
| `CREATE FULLTEXT INDEX`, `CREATE ANALYZER` | `CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];` | [11](11-full-text-search.md) | Phase 3 |
| `CREATE POINT INDEX` | `CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);` | [12](12-geospatial.md) | Phase 3 |
| `ALTER TABLE ... CHANGEFEED` | `ALTER TABLE Person CHANGEFEED 7d;` | [14](14-live-queries-events.md) | Phase 4 |
| `LIVE MATCH`, `KILL LIVE` | `LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o;` | [14](14-live-queries-events.md) | Phase 4 |
| `CREATE EVENT ... WHEN ... THEN { ... }` | `CREATE EVENT order_shipped ON Order WHEN ... THEN { ... };` | [14](14-live-queries-events.md) | Phase 4 |
| `CREATE USER`, `CREATE ACCESS`, `ALTER TABLE ... PERMISSIONS` | `CREATE USER analyst ON DATABASE PASSWORD $pw ROLES [viewer];` | [18](18-security-auth.md) | Phase 5 |

#### Procedures

| Procedure | Yields | Owner | Status |
|---|---|---|---|
| `db.vector.search(indexName, vector, k[, thresholdOrOptions])` | `node, score` | [10](10-vector-index.md) | Implemented (filtering options Phase 3) |
| `db.labels`, `db.relationshipTypes`, `db.indexes` | see [Procedures](#procedures) | this chapter | Implemented |
| `db.text.search(index, query, k)` | `node, score` | [11](11-full-text-search.md) | Phase 3 |
| `db.search.hybrid({text, vector}, query, vector, k)` | `node, score` (RRF) | [11](11-full-text-search.md) | Phase 3 |
| `db.kv.set`, `db.kv.get`, `db.kv.delete`, `db.kv.scan` | see owner | [13](13-key-value-time-series.md) | Phase 3 |
| `algo.pagerank`, `algo.wcc` | see owner | [09](09-graph-model.md) | Phase 3 |
| `db.changes(table, {since})` | `change` | [14](14-live-queries-events.md) | Phase 4 |

#### Functions

| Function | Purpose | Owner | Status |
|---|---|---|---|
| `db.vector.similarity(a, b)` | Cosine similarity of two vectors | [10](10-vector-index.md) | Implemented |
| `id(x)`, `rid(table, key)` | Return or construct a `RecordId` | [03](03-data-model.md) | Implemented |
| `time.now()`, `datetime(...)`, `duration('PT5M')` | Datetime and duration values | [03](03-data-model.md) | Implemented |
| `text.highlight(...)` | Highlight matched terms | [11](11-full-text-search.md) | Phase 3 |
| `point({longitude, latitude})`, `point.distance` | Point construction and great-circle distance in metres | [12](12-geospatial.md) | Implemented |
| `geo.within`, `geo.intersects`, `geo.polygon` | Geometry predicates and construction | [12](12-geospatial.md) | Phase 3 |
| `ts.bucket(...)` | Time bucketing for downsampling | [13](13-key-value-time-series.md) | Phase 3 |
| `crypto.argon2.generate`, `crypto.argon2.compare` | Password hashing in access methods | [18](18-security-auth.md) | Phase 5 |

### Multi-Statement Scripts (Implemented; `USE` Phase 2)

Scripts and `BEGIN` / `COMMIT` / `ROLLBACK` work today (see
[Scripts and transactions](#scripts-and-transactions)). `USE` arrives with Phase 2 and
changes the session context for the following statements:

```cypher
USE acme.shop;                                                    // Phase 2
BEGIN;
MATCH (p:Product {sku: $sku}) SET p.stock = p.stock - $qty;
CREATE (:Order {sku: $sku, qty: $qty, at: time.now()});
COMMIT;
```

Today a script returns only the result of its last statement; returning one result set
per statement is planned with the server protocols (Phase 5).

### Parameters

`$name` binds a value supplied with the query, through
`Engine::execute(cypher, &[("name", value)])` or `Transaction::execute`.

- Parameters may hold any `Value`, including `Object`, `RecordId`, `Vector`,
  `Datetime`, and `Geometry`. A `RecordId` parameter is the canonical way to address a
  record by id, alongside `rid('Person', 'alice')`.
- Parameters are values only. Labels, table names, and property keys cannot be
  parameterised, as in Neo4j, so query plans stay cacheable.
- Session parameters `$auth`, `$session`, `$access`, and `$token` are reserved and set
  by the server (Phase 5; see [18 — Security & Auth](18-security-auth.md)).
- In `ASSERT`, `value` names the field value being written; in events, `$event`,
  `$before`, and `$after` describe the change.

### Compatibility Notes

| Topic | Neo4j / openCypher | Atlantis Cypher |
|---|---|---|
| Inequality | `<>` | `<>`; `!=` is accepted as an alias. |
| Node identity | `elementId(n)` string; `id(n)` integer (deprecated) | `id(n)` and `n.id` return a `RecordId` such as `Person:alice`; `elementId(n)` returns its text. The pattern property `{id: ...}` matches or sets the record key. |
| Labels | Any number, all equivalent | The **first label is the table** and cannot change. Further labels are secondary labels. `SET n:Tag` and `REMOVE n:Tag` affect secondary labels only. Unlabeled nodes go to the `_default` table. |
| Relationship types | Free-form | Each type is an edge table; it may be declared with schema (`TYPE EDGE`). |
| Property values | Scalars, lists of scalars | Full documents: nested objects and arrays, plus `Vector`, `Geometry`, `Datetime`, `RecordId`. A stored `RecordId` is a link that property access follows. |
| Nested writes | Not supported | `SET p.address.city = 'Hue'`. |
| Schema | Constraints and indexes only | Adds tables, typed fields, `SCHEMAFULL` mode ([07](07-schema-ddl.md)). |
| Procedures | `CALL ... YIELD` | Same; Atlantis procedure namespaces are `db.*` (including `db.kv.*`) and `algo.*`. |
| Multi-database | `USE db` per query, `CREATE DATABASE` | Two levels: `USE ns.db`; `CREATE NAMESPACE`, `CREATE DATABASE ns.db`. |
| Bolt protocol | Native | Optional server feature on port 7687 (Phase 6; see [17](17-server-protocols.md)). |
| `CALL { }` and `EXISTS { }` subqueries, map projections, `FOREACH`, `LOAD CSV`, quantified path patterns | Supported in Neo4j 5 | Not implemented and not in the current roadmap. |

## Design Decisions

**Why extend Cypher instead of adding a second language?**
One language means one parser, one executor, one set of client tools, and no
translation layer between models. Cypher's pattern syntax already covers the graph
case, and its clause pipeline (`MATCH` → `WITH` → `RETURN`) handles documents and rows
well. Only schema, search, and reactive features need new syntax.

**Why procedures for search and key-value?**
`CALL ... YIELD` composes with later clauses, is the form Neo4j users already know for
vector search, and needs no new grammar per feature. A new index type adds a procedure,
not a keyword.

**Why a hand-written recursive-descent parser?**
It gives full control over error messages and Cypher's syntax quirks (arrows, inline
properties, variable-length ranges), and new extension statements can be added as
plain functions.

**Why case-insensitive keywords?**
Cypher treats keywords case-insensitively. Identifiers, labels, and property keys keep
their case.

## What's Next

- **[07 — Schema & DDL](07-schema-ddl.md)** — Namespaces, tables, fields, indexes, and constraints.
- **[08 — Document & Relational Model](08-document-relational.md)** — Nested documents, links, joins, aggregation.
- **[09 — Graph Model](09-graph-model.md)** — Edges, traversal, paths, and graph algorithms.
- **[15 — Query Planner & Executor](15-query-planner-executor.md)** — How the AST runs today and the planned planner.
- **[23 — Query Cookbook](23-query-cookbook.md)** — Recipes per model.
