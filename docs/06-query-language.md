# 06 — Atlantis Cypher Query Language

> **Status:** Partially implemented — a read-mostly Cypher subset works today; the dialect extensions are Planned — Phases 0–7
> **Crates:** `atlantis-cypher`, `atlantis-engine`

Atlantis DB has one query language: **Atlantis Cypher**. It is Cypher as defined by
Neo4j 5, openCypher, and GQL, plus extensions for the models Cypher has no syntax for
(schema, documents, search, key-value, time-series, live queries, security). Where
Cypher already has a form, Atlantis Cypher uses it unchanged.

This chapter covers two things:

1. **Implemented today** — the exact subset the lexer, parser, planner, and executor
   accept now, including known gaps, so you can write queries that actually work.
2. **Target design** — the full extension catalogue, with the chapter that owns each
   construct and the roadmap phase that delivers it.

## Implemented Today

The behaviour below was verified by reading the source and by running queries. (It was
first checked on a copy patched to compile; the workspace itself now builds.)

### Pipeline

A query string goes through four stages, all in-process:

```
query text ─► tokenize ─► parse ─► plan ─► optimize ─► execute ─► QueryResult
              lexer/      parser/   planner/  planner/      atlantis-engine
              tokenizer   statement mod.rs    optimizer     executor.rs
```

The entry points are `atlantis_cypher::parse`, `atlantis_cypher::plan`, and
`atlantis_cypher::optimize` (re-exported from `crates/atlantis-cypher/src/lib.rs`).
`QueryExecutor::execute` in `crates/atlantis-engine/src/executor.rs` runs all stages.
The optimizer is a pass-through today; see
[15 — Query Planner & Executor](15-query-planner-executor.md).

One call accepts one statement. There is no `;` token, so a statement separator is a
lexer error.

### Clause Support

"Parsed" means the parser accepts the clause. "Executed" means the planner and executor
give the documented Cypher result. The status column is about Cypher semantics, not
about whether a query returns without an error.

| Clause | Parsed | Executed | Notes on today's behaviour |
|---|---|---|---|
| `MATCH (n)` / `MATCH (n:Label)` | Yes | Yes | Scans all nodes, or the nodes with the **first** label. Extra labels are ignored. |
| `MATCH` with inline properties `(n {k: v})` | Yes | No | The planner drops inline properties, so every node with the label matches. Use `WHERE`. |
| `MATCH` with a relationship | Partial | No | Only `-->` and `-[...]->` parse. The expand follows outgoing edges of the given type (an untyped relationship returns no rows), but the next node pattern rescans its label and discards the expand rows, so the far node binds to every node with that label. See [09 — Graph Model](09-graph-model.md). |
| Comma-separated patterns `MATCH (a), (b)` | No | No | The parser stops at the comma. |
| Several `MATCH` clauses | Yes | No | Each scan ignores its input. Only the last scan's rows reach `RETURN`. |
| `OPTIONAL MATCH` | Yes | No | The planner treats it as `MATCH`. No null rows are produced. |
| `WHERE` | Yes | Yes | Rows pass only when the predicate is exactly `true`. |
| `CREATE (n:Label {...})` | Yes | Yes | Creates nodes. Property values may use literals and `$params`. |
| `CREATE` with a relationship | Yes | No | Both nodes are created; the relationship element is dropped by the planner. Only the last created node is returned. |
| `RETURN` | Yes | Yes | Projection with `AS` aliases and `*`. The clause ends parsing, so any tokens after it are ignored. As the first clause (`RETURN 1`) it returns no rows. |
| `RETURN DISTINCT` | Yes | No | The flag is parsed and ignored. |
| `ORDER BY` | Yes | Partial | Sorts after projection, so sort by a returned alias or variable. `DESC`/`DESCENDING` must be uppercase; `ASC` is not recognised. |
| `SKIP`, `LIMIT` | Yes | Yes | Must appear in the order `SKIP` then `LIMIT`. Values may be literals or `$params`. |
| `WITH ... [WHERE ...]` | Yes | Partial | Projection and `WHERE` work. `DISTINCT`, `ORDER BY`, `SKIP`, and `LIMIT` on `WITH` are ignored. |
| `UNWIND list AS x` | Yes | Partial | Works after a clause that produced rows. As the first clause it produces no rows. |
| `SET n.prop = value` | No | No | The parser reads `n.prop = value` as one comparison and then fails with `expected Equals`. Even past the parser, the planner builds the update on an empty input, so `MATCH ... SET` changes nothing. |
| `DELETE n` | Yes | Yes | Marks the node deleted. Neighbour lookups skip deleted targets. |
| `DETACH DELETE n` | Yes | Partial | Behaves like `DELETE`; edges are not removed. |
| `CALL db.vector.search(...) YIELD ...` | Yes | No | Plans and runs, but the engine never inserts vectors into the HNSW index, so it returns no rows. A later `MATCH` would discard its rows. See [10 — Vector Index](10-vector-index.md). |
| `MERGE`, `REMOVE`, `UNION` | No | No | The tokens exist, but the parser rejects `MERGE` and `REMOVE`. `UNION` after `RETURN` is silently ignored. |

### Expressions

| Construct | Status today |
|---|---|
| Literals: integer, float, single- or double-quoted string, `true`, `false`, `null` | Implemented |
| List literal `[1, 2, n.x]` | Implemented (evaluates to `Array`) |
| Map literal `{a: 1}` | Parsed; evaluates to `null` because `Value` has no map type yet |
| Parameters `$name` | Implemented. An unbound variable name also falls back to a parameter of the same name. |
| Property access `n.prop` | Implemented, one level only, on a variable. Reads the hot tier. |
| `=`, `!=`, `<`, `>`, `<=`, `>=` | Implemented |
| `<>` | Not working: the lexer emits `<` then `>`. Use `!=` today. |
| `AND`, `OR`, `NOT` | Implemented. If either side reads a missing property, the whole expression is `null` and the row is filtered out. |
| `XOR`, `IN`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `IS [NOT] NULL` | AST variants exist; the parser does not produce them |
| `+`, `-`, `*`, `/` | Implemented for integer–integer and float–float pairs; `+` also concatenates strings. Mixed types give `null`. |
| `%` | Parsed; evaluates to `null` |
| `CASE ... END` | Not parsed (`CASE` is not a keyword) |
| Function calls `f(x)` | Parsed for undotted names; the executor knows no functions, so the result is `null`. `count(*)` does not parse. |
| Dotted function `db.vector.similarity(a, b)` | The executor implements it, but the parser reads `db.vector` as property access and fails on the next dot |

Write a space before a minus sign that follows a value: `n.age - 1` works, while
`n.age-1` lexes `-1` as a negative literal and the rest of the `RETURN` item is dropped.

### Results

`QueryResult` holds `columns` and `rows`. A node variable projects to its integer
`NodeId`, not to a node object. A column takes its alias, else the variable name, else
`var.prop`, else `expr`. Typed accessors live in `ResultSet`; see
[21 — API Reference](21-api-reference.md).

### Lexer

**Files:** `crates/atlantis-cypher/src/lexer/token.rs`, `crates/atlantis-cypher/src/lexer/tokenizer.rs`

The tokenizer turns the query into `SpannedToken { token, span }` values, where `span`
is a byte range, and ends the stream with `Token::Eof`.

- **Keywords** are case-insensitive. The keyword list is the `KEYWORDS` constant in
  `tokenizer.rs`. Words outside it, including `DESC`, `CASE`, `ASC`, and all extension
  words, are identifiers and keep their original case.
- **Strings** use single or double quotes and support the escapes `\n`, `\t`, `\r`,
  `\\`, `\'`, `\"`. Any other escaped character is kept as-is.
- **Numbers**: a run of digits and dots is one number, so `1..3` is lexed as an invalid
  float. A `-` directly before a digit becomes part of the literal.
- **Parameters**: `$name` gives `Token::Parameter("name")`.
- **Comments**: `// line` and `/* block */` are skipped.
- **Arrows**: `->` is `ArrowRight`; `<->` is `ArrowBoth`. `-->` lexes as `-` then `->`.
  `<-` is lexed as `<` then `-`, so the `ArrowLeft` token is never produced.
- **Errors** are `AtlantisError::Parse` for an unterminated string, an unexpected
  character (including `;` and a lone `!`), or an unparsable number.

### Parser

**File:** `crates/atlantis-cypher/src/parser/statement.rs`

The parser is hand-written recursive descent over the token slice. `chumsky` is a
declared dependency of the crate but is not used. `clause.rs`, `expression.rs`, and
`pattern.rs` only re-export AST types.

Expression precedence, lowest to highest:

```
OR
AND
comparison   = != < > <= >=     (left-associative, chainable)
additive     + -
multiplicative * / %
unary        - NOT              (applies to a primary, not a full expression)
primary      literal | $param | var | var.prop | f(args) | ( expr ) | [list] | {map}
```

Because unary `NOT` binds to a primary, write `NOT (a = 1)` when you mean the
negation of a comparison.

### AST

**File:** `crates/atlantis-cypher/src/parser/ast.rs`

`Statement` has one variant, `Query { clauses, result }`. `Clause` covers `Match`,
`Where`, `Create`, `Merge`, `Delete`, `Set`, `Remove`, `With`, `Unwind`, and `Call`;
the `Merge` and `Remove` variants are never produced by the parser today. A `Pattern`
is an alternating list of `NodePattern` and `RelationshipPattern` elements; a
relationship carries an optional `PathLength { min, max }`. `Expression`, `BinaryOp`,
and `UnaryOp` are defined in the same file; read it for the exact variant list rather
than relying on a copy here.

### Working Examples

These run end to end on today's executor:

```cypher
MATCH (n:Person) RETURN n.name AS name ORDER BY name DESC LIMIT 10
```

```cypher
MATCH (n:Person) WHERE n.name = $name AND n.age >= 18 RETURN n.age AS age
```

```cypher
MATCH (n:Person) WITH n.name AS name WHERE name != 'Bob' RETURN name
```

```cypher
CREATE (n:Person {name: $name, age: 30}) RETURN n
```

```cypher
MATCH (n:Person) WHERE n.name = 'Carol' DELETE n
```

```cypher
CALL db.vector.search('Chunk', $query_vec, 10, 0.7) YIELD node, score
RETURN node, score
```

The last example parses and plans but returns no rows today, because the engine never
populates the HNSW index. Its arguments must be a string literal label, the query
vector, an integer literal `k`, and an optional numeric literal threshold. The label is
ignored, the first yielded column holds the internal integer index id (not a node), and
`score` is `1 - distance`. The target signature takes an index name instead of a label:
`db.vector.search(indexName, vector, k[, thresholdOrOptions])`.

## Target Design: Atlantis Cypher (Planned)

### Language Principles

- **Cypher first.** Any construct that exists in Neo4j 5, openCypher, or GQL keeps that
  syntax and meaning: `MERGE ... ON CREATE SET`, `OPTIONAL MATCH`, `UNION`, `CASE`,
  aggregation with implicit grouping, list comprehensions, map projections, path
  variables, `shortestPath`, `CREATE INDEX ... FOR ... ON`, `CREATE CONSTRAINT ...
  REQUIRE`, `SHOW INDEXES`.
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
| Complete core Cypher (`MERGE`, `SET`, `REMOVE`, `OPTIONAL MATCH`, `UNION`, `CASE`, `IN`, `IS NULL`, string predicates, `<>`, incoming and undirected patterns, comma patterns) | `MERGE (p:Person {email: $e}) ON CREATE SET p.created = time.now()` | this chapter, [08](08-document-relational.md), [09](09-graph-model.md) | Phase 0 |
| Relationship `CREATE`, `DETACH DELETE`, variable-length paths | `MATCH (a)-[:KNOWS*1..3]->(b)` | [09](09-graph-model.md) | Phase 0 |
| Aggregation and grouping | `RETURN c.name, count(o) AS orders` | [08](08-document-relational.md) | Phase 0 |
| Path variables and path functions | `MATCH p = (a)-[:KNOWS*1..3]->(b) RETURN length(p)` | [09](09-graph-model.md) | Phase 0 |
| `shortestPath`, `allShortestPaths` | `MATCH p = shortestPath((a)-[:KNOWS*]-(b))` | [09](09-graph-model.md) | Phase 3 |
| Multi-statement scripts, `BEGIN` / `COMMIT` / `ROLLBACK` | `BEGIN; ...; COMMIT;` | [05](05-transactions-consistency.md) | Phase 1 |
| `CREATE NAMESPACE`, `CREATE DATABASE`, `USE` | `USE acme.shop;` | [07](07-schema-ddl.md) | Phase 2 |
| `CREATE TABLE ... SCHEMAFULL / SCHEMALESS`, `TYPE EDGE` | `CREATE TABLE WORKS_AT TYPE EDGE FROM Person TO Company;` | [07](07-schema-ddl.md) | Phase 2 |
| `CREATE FIELD` with `TYPE`, `DEFAULT`, `ASSERT`, `COMPUTED`, `READONLY` | `CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';` | [07](07-schema-ddl.md) | Phase 2 |
| `ALTER TABLE`, `DROP TABLE`, `SHOW ...` introspection | `SHOW TABLES` | [07](07-schema-ddl.md) | Phase 2 |
| `CREATE INDEX`, `CREATE CONSTRAINT` (Neo4j 5 forms) | `CREATE INDEX person_email FOR (p:Person) ON (p.email);` | [07](07-schema-ddl.md), [08](08-document-relational.md) | Phase 2 |
| Nested document paths in `SET` and `REMOVE` | `SET p.address.city = 'Hue'` | [08](08-document-relational.md) | Phase 2 |
| `CREATE TABLE ... TYPE TIMESERIES` | `CREATE TABLE Reading TYPE TIMESERIES KEY (sensor, ts) RETENTION 30d;` | [13](13-key-value-time-series.md) | Phase 3 |
| `CREATE VECTOR INDEX` | `... OPTIONS {dimension: 1536, metric: 'cosine'}` | [10](10-vector-index.md) | Phase 3 |
| `CREATE FULLTEXT INDEX`, `CREATE ANALYZER` | `CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];` | [11](11-full-text-search.md) | Phase 3 |
| `CREATE POINT INDEX` | `CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);` | [12](12-geospatial.md) | Phase 3 |
| `ALTER TABLE ... CHANGEFEED` | `ALTER TABLE Person CHANGEFEED 7d;` | [14](14-live-queries-events.md) | Phase 4 |
| `LIVE MATCH`, `KILL LIVE` | `LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o;` | [14](14-live-queries-events.md) | Phase 4 |
| `CREATE EVENT ... WHEN ... THEN { ... }` | `CREATE EVENT order_shipped ON Order WHEN ... THEN { ... };` | [14](14-live-queries-events.md) | Phase 4 |
| `CREATE USER`, `CREATE ACCESS`, `ALTER TABLE ... PERMISSIONS` | `CREATE USER analyst ON DATABASE PASSWORD $pw ROLES [viewer];` | [18](18-security-auth.md) | Phase 5 |

#### Procedures

| Procedure | Yields | Owner | Status |
|---|---|---|---|
| `db.vector.search(indexName, vector, k[, thresholdOrOptions])` | `node, score` | [10](10-vector-index.md) | Partial (label-based form parses today; index is never populated) |
| `db.text.search(index, query, k)` | `node, score` | [11](11-full-text-search.md) | Phase 3 |
| `db.search.hybrid({text, vector}, query, vector, k)` | `node, score` (RRF) | [11](11-full-text-search.md) | Phase 3 |
| `db.kv.set`, `db.kv.get`, `db.kv.delete`, `db.kv.scan` | see owner | [13](13-key-value-time-series.md) | Phase 3 |
| `algo.pagerank`, `algo.wcc` | see owner | [09](09-graph-model.md) | Phase 3 |
| `db.changes(table, {since})` | `change` | [14](14-live-queries-events.md) | Phase 4 |

#### Functions

| Function | Purpose | Owner | Status |
|---|---|---|---|
| `db.vector.similarity(a, b)` | Cosine similarity of two vectors | [10](10-vector-index.md) | Partial (executor only; parser gap is Phase 0) |
| `id(x)`, `rid(table, key)` | Return or construct a `RecordId` | [03](03-data-model.md) | Phase 2 |
| `time.now()`, `duration('PT5M')` | Datetime and duration values | [03](03-data-model.md) | Phase 2 |
| `text.highlight(...)` | Highlight matched terms | [11](11-full-text-search.md) | Phase 3 |
| `point({longitude, latitude})`, `point.distance`, `geo.within`, `geo.intersects`, `geo.polygon` | Geometry construction and predicates | [12](12-geospatial.md) | Phase 3 |
| `ts.bucket(...)` | Time bucketing for downsampling | [13](13-key-value-time-series.md) | Phase 3 |
| `crypto.argon2.generate`, `crypto.argon2.compare` | Password hashing in access methods | [18](18-security-auth.md) | Phase 5 |

### Multi-Statement Scripts (Planned — Phase 1)

A script is a sequence of statements separated by `;`. The trailing `;` is optional.
Statements run in order in one session, and each produces its own result set.

```cypher
USE acme.shop;
BEGIN;
MATCH (p:Product {sku: $sku}) SET p.stock = p.stock - $qty;
CREATE (:Order {sku: $sku, qty: $qty, at: time.now()});
COMMIT;
```

Outside `BEGIN` … `COMMIT`, each statement commits on its own. Inside, all statements
commit atomically or not at all; `ROLLBACK` discards them, and an error aborts the
whole transaction. Isolation and conflict rules are in
[05 — Transactions & Consistency](05-transactions-consistency.md). `USE` changes the
session context for the following statements.

### Parameters

`$name` binds a value supplied with the query. Parameters work today through
`Engine::execute(cypher, &[("name", value)])`. In the target design:

- Parameters may hold any `Value`, including `Object`, `RecordId`, `Datetime`, and
  `Geometry` (Phase 2–3). A `RecordId` parameter is the canonical way to address a
  record by id, alongside `rid('Person', 'alice')`.
- Parameters are values only. Labels, table names, and property keys cannot be
  parameterised, as in Neo4j, so query plans stay cacheable.
- Session parameters `$auth`, `$session`, `$access`, and `$token` are reserved and set
  by the server (Phase 5; see [18 — Security & Auth](18-security-auth.md)).
- In `ASSERT`, `value` names the field value being written; in events, `$event`,
  `$before`, and `$after` describe the change.

The bare-variable fallback to a parameter of the same name is an implementation
accident of today's executor. Planned behaviour: an unbound variable is an error.

### Compatibility Notes

| Topic | Neo4j / openCypher | Atlantis Cypher |
|---|---|---|
| Inequality | `<>` | `<>` (Phase 0). `!=` works today and stays accepted as an alias. |
| Node identity | `elementId(n)` string; `id(n)` integer (deprecated) | `id(n)` returns a `RecordId` such as `Person:alice` (Phase 2). Today a node variable is an integer `NodeId`. |
| Labels | Any number, all equivalent | The **first label is the table** and cannot change. Further labels are secondary tags. `SET n:Tag` and `REMOVE n:Tag` affect secondary labels only (Phase 2). |
| Relationship types | Free-form | Each type is an edge table; it may be declared with schema (`TYPE EDGE`). |
| Property values | Scalars, lists of scalars | Full documents: nested objects and arrays, plus `Vector`, `Geometry`, `Datetime`, `RecordId` (Phase 2–3). |
| Nested writes | Not supported | `SET p.address.city = 'Hue'` (Phase 2). |
| Schema | Constraints and indexes only | Adds tables, typed fields, `SCHEMAFULL` mode ([07](07-schema-ddl.md)). |
| Procedures | `CALL ... YIELD` | Same; Atlantis procedure namespaces are `db.*` (including `db.kv.*`) and `algo.*`. |
| Multi-database | `USE db` per query, `CREATE DATABASE` | Two levels: `USE ns.db`; `CREATE NAMESPACE`, `CREATE DATABASE ns.db`. |
| Bolt protocol | Native | Optional server feature on port 7687 (Phase 6; see [17](17-server-protocols.md)). |
| `CALL { }` subqueries, quantified path patterns | Supported in Neo4j 5 | Not in the current roadmap. |

## Design Decisions

**Why extend Cypher instead of adding a second language?**
One language means one parser, one planner, one set of client tools, and no
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
plain functions. The `chumsky` dependency is unused.

**Why case-insensitive keywords?**
Cypher treats keywords case-insensitively. Identifiers, labels, and property keys keep
their case.

## What's Next

- **[07 — Schema & DDL](07-schema-ddl.md)** — Namespaces, tables, fields, indexes, and constraints.
- **[08 — Document & Relational Model](08-document-relational.md)** — Nested documents, links, joins, aggregation.
- **[09 — Graph Model](09-graph-model.md)** — Edges, traversal, paths, and graph algorithms.
- **[15 — Query Planner & Executor](15-query-planner-executor.md)** — How the AST becomes a plan and runs.
- **[23 — Query Cookbook](23-query-cookbook.md)** — Recipes per model.
