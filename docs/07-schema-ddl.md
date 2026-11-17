# 07 — Schema & DDL

> **Status:** Partially implemented — vector and property indexes, node constraints (`UNIQUE`, `NOT NULL`, `NODE KEY`), `DROP`, `SHOW INDEXES`, `SHOW CONSTRAINTS` and `EXPLAIN` are implemented; the rest is Planned — Phase 2 (time-series tables, full-text and point indexes: Phase 3; change feeds: Phase 4; permissions: Phase 5)
> **Crates:** `atlantis-cypher`, `atlantis-engine`, `atlantis-storage`, `atlantis-kv`

This chapter owns the data definition language (DDL) of Atlantis Cypher: namespaces,
databases, tables, fields, types, constraints, indexes, analyzers, and introspection.
Other chapters own the behaviour behind some clauses and link back here for the
grammar.

## Implemented Today

Index, constraint and introspection statements, in the Neo4j 5 forms:

```
CREATE [RANGE] INDEX [name] [IF NOT EXISTS] FOR (n:Label) ON (n.prop [, n.nested.prop ...])
CREATE VECTOR INDEX name [IF NOT EXISTS] FOR (n:Label) ON (n.prop)
  [OPTIONS {dimension: N, metric: 'cosine' | 'euclidean' | 'dot_product', m: M, efConstruction: E}]
CREATE CONSTRAINT [name] [IF NOT EXISTS] FOR (n:Label)
  REQUIRE n.prop IS [NODE] UNIQUE
        | (n.prop, n.prop) IS [NODE] UNIQUE
        | n.prop IS NOT NULL
        | (n.prop, n.prop) IS [NODE] KEY
DROP INDEX name [IF EXISTS]
DROP CONSTRAINT name [IF EXISTS]
SHOW [ALL] INDEXES
SHOW [ALL] CONSTRAINTS
EXPLAIN query
```

```cypher
CREATE INDEX person_email FOR (p:Person) ON (p.email);
CREATE INDEX FOR (p:Person) ON (p.address.city);          -- named index_Person_address_city
CREATE CONSTRAINT person_email_unique FOR (p:Person) REQUIRE p.email IS UNIQUE;
CREATE CONSTRAINT FOR (b:Book) REQUIRE (b.isbn, b.edition) IS NODE KEY;
```

- **Names** are optional; a missing name is generated as `index_{Label}_{props}` or
  `constraint_{Label}_{props}_{unique|not_null|node_key}`. Indexes and constraints
  share one namespace with vector indexes.
- **`IF NOT EXISTS`** succeeds without change when the name is taken or an equivalent
  definition (same label and properties, and for constraints the same kind) exists.
  Without it, both are errors.
- **Builds are synchronous.** `CREATE INDEX` and `CREATE CONSTRAINT` read every node
  with the label and write their entries in the same transaction, so a new index is
  complete the moment it commits; there is no `POPULATING` state. A build conflicts with
  writes to that label that commit while it runs; the later commit gets `TxConflict`
  and can retry.
- **Constraints check existing data first.** If a node breaks the constraint, the
  statement fails with `ConstraintViolation` naming it, and nothing is created.
- `DROP INDEX` drops a vector or property index; the index of a unique constraint goes
  with `DROP CONSTRAINT`.

Definitions live in the catalog (`/sys/.../ix/{name}` for vector indexes,
`/sys/.../sc/{name}` for the others; [04 — Storage Engine](04-storage-engine.md)).
Query use is owned by [08 — Document & Relational](08-document-relational.md#secondary-indexes)
and [15 — Query Planner & Executor](15-query-planner-executor.md); vector search by
[10 — Vector Index](10-vector-index.md). `CALL db.indexes()` lists vector indexes only;
`SHOW INDEXES` lists every kind.

Not yet: relationship indexes and constraints (`FOR ()-[r:TYPE]-()`), `TEXT`, `POINT`
and `FULLTEXT` index kinds, type constraints, and `YIELD`/`WHERE` after `SHOW`.

Everything else in this chapter is target design. What exists without DDL:

- **Schemaless tables.** A node's first label names its table and a relationship type
  names its edge table; tables are created on first write. Property values are not
  type-checked. Unlabeled nodes go to the `_default` table.
- **Namespaces and databases** exist in the record store and are selected from Rust
  with `Engine::use_db(ns, db)`; there is no `CREATE NAMESPACE`, `CREATE DATABASE` or
  `USE` statement yet.
- **Label index.** Every label (table or secondary) has an index used by
  `MATCH (n:Label)`.

## Grammar Conventions

Grammar blocks use `[ ]` for optional parts, `|` for alternatives, `{ }...` for
repetition, and `UPPERCASE` for keywords. Keywords are case-insensitive. Names
(namespaces, tables, fields, indexes) are case-sensitive identifiers. Durations in DDL
use a number and a unit: `ms`, `s`, `m`, `h`, `d`, `w` (for example `30d`, `15m`).

Every `CREATE` statement accepts `IF NOT EXISTS`, and every `DROP` statement accepts
`IF EXISTS`, as in Neo4j. Without them, creating an existing object or dropping a
missing one is an error.

## Namespaces and Databases (Planned — Phase 2)

The hierarchy is **Namespace → Database → Table → Record**. A namespace groups
databases for one tenant or team. A database is the unit of isolation for schema,
data, transactions, and change feeds.

```
CREATE NAMESPACE [IF NOT EXISTS] ns
CREATE DATABASE  [IF NOT EXISTS] ns.db
DROP NAMESPACE   [IF EXISTS] ns
DROP DATABASE    [IF EXISTS] ns.db
USE ns.db
```

```cypher
CREATE NAMESPACE acme;
CREATE DATABASE acme.shop;
USE acme.shop;
```

- `USE` sets the session context for the following statements. On the server the
  context can also come from the `Atlantis-NS` and `Atlantis-DB` HTTP headers or the
  `use` RPC method ([17 — Server & Protocols](17-server-protocols.md)).
- An embedded engine starts in namespace `atlantis`, database `default`.
- Dropping a namespace or database deletes everything in it. It requires the `owner`
  role at the level above ([18 — Security & Auth](18-security-auth.md)).

## Tables (Planned — Phase 2)

Every record belongs to exactly one table. In graph terms, a node's first label is its
table, and a relationship type is an edge table ([09 — Graph Model](09-graph-model.md)).

```
CREATE TABLE [IF NOT EXISTS] name
    [ TYPE EDGE [FROM table] [TO table]
    | TYPE TIMESERIES KEY (field, field) [RETENTION duration] ]
    [ SCHEMAFULL | SCHEMALESS ]
    [ ASSERT expression ]
    [ CHANGEFEED duration ]
    [ PERMISSIONS permission-rules ]

ALTER TABLE name
    { SCHEMAFULL | SCHEMALESS | ASSERT expression | CHANGEFEED duration
    | PERMISSIONS permission-rules }

DROP TABLE [IF EXISTS] name
```

```cypher
CREATE TABLE Person SCHEMAFULL;
CREATE TABLE Event SCHEMALESS;
CREATE TABLE WORKS_AT TYPE EDGE FROM Person TO Company;
CREATE TABLE Reading TYPE TIMESERIES KEY (sensor, ts) RETENTION 30d;
ALTER TABLE Person CHANGEFEED 7d;
DROP TABLE Event;
```

### Table kinds

| Kind | Declared with | Record keys | Owner chapter | Status |
|---|---|---|---|---|
| Normal | no `TYPE` clause | ULID by default; integer, string, UUID, or array keys allowed | [03](03-data-model.md), [08](08-document-relational.md) | Phase 2 |
| Edge | `TYPE EDGE [FROM A] [TO B]` | ULID; every record has `in` and `out` `RecordId`s | [09](09-graph-model.md) | Phase 2 |
| Time-series | `TYPE TIMESERIES KEY (series, time)` | Composite array key `[series, time]` | [13](13-key-value-time-series.md) | Phase 3 |

`FROM` and `TO` restrict which tables an edge may connect. Omit either to allow any
table on that side.

A table-level `ASSERT` checks the whole record after all field rules have run, for
rules that span fields, for example
`CREATE TABLE Booking ASSERT b.end > b.start;`.

### Record keys

Writing an `id` property sets the record key explicitly; otherwise a ULID is
generated. Both of these create the record `Person:alice`:

```cypher
CREATE (:Person {id: 'alice', name: 'Alice'});
CREATE (:Person {id: rid('Person', 'alice'), name: 'Alice'});
```

A `rid()` value must name the same table. The key cannot be changed after creation.

### Record variables in table-scoped expressions

`COMPUTED`, table-level `ASSERT`, and `PERMISSIONS` expressions run against one
record. The current record is bound to the single free variable the expression uses
(`p` in `p.first + ' ' + p.last`); an expression that uses two different free variables
is rejected. `this` is always accepted as an alias for the current record. In a field
`ASSERT`, `value` is the field's value instead.

### Schemaless and schemafull

| Mode | Undeclared fields | Declared fields |
|---|---|---|
| `SCHEMALESS` (default) | Stored as written | Type, default, and assertions enforced |
| `SCHEMAFULL` | Rejected; the write fails | Enforced; non-`option<T>` fields are required |

A table that has not been created explicitly is created as `SCHEMALESS` the first time a
record is written to it, so plain Cypher such as `CREATE (:Person {name: 'Ana'})` keeps
working without any DDL. A node created without a label, such as `CREATE ({note: 'x'})`,
goes into the reserved schemaless table `_default`. Table names starting with `_` are
reserved for the system and cannot be created by users. Declaring fields on a schemaless table is the usual first step
toward a stricter schema.

### Dropping a table

`DROP TABLE` removes the table's records, fields, indexes, constraints, events, and
change feed. Dropping a node table that still has edges fails unless the edges are
deleted first with `DETACH DELETE`, so no edge points at a missing record.

## Fields (Planned — Phase 2)

```
CREATE FIELD [IF NOT EXISTS] path ON table
    [ TYPE type ]
    [ DEFAULT expression ]
    [ ASSERT expression ]
    [ READONLY ]
    [ PERMISSIONS permission-rules ]
  | COMPUTED expression

ALTER FIELD path ON table  { TYPE type | DEFAULT expression | ASSERT expression | READONLY }
DROP FIELD [IF EXISTS] path ON table
```

```cypher
CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';
CREATE FIELD age ON Person TYPE option<int> DEFAULT 0 ASSERT value >= 0;
CREATE FIELD fullName ON Person COMPUTED p.first + ' ' + p.last;
CREATE FIELD address.city ON Person TYPE option<string>;
CREATE FIELD customer ON Order TYPE record<Customer>;
CREATE FIELD tags ON Post TYPE array<string> DEFAULT [];
CREATE FIELD createdAt ON Post TYPE datetime DEFAULT time.now() READONLY;
```

`path` is a field name or a dotted path into a nested object (`address.city`).

### Write-time order

On every create or update, each declared field is processed in this order:

1. **DEFAULT** fills the value when the field is absent. It does not replace an
   explicit `null`.
2. **TYPE** checks the value. Integer values are widened to `float` and `decimal`
   fields. No other coercion happens; for example a string is never parsed into a
   number.
3. **ASSERT** evaluates a boolean expression. `value` is the new field value. Anything
   other than `true` rejects the write.
4. **READONLY** rejects an update that changes a value set at creation.

A rejected write fails the whole statement, and inside an explicit transaction it
aborts the transaction ([05 — Transactions & Consistency](05-transactions-consistency.md)).

### Computed fields

A `COMPUTED` field is evaluated when it is read and is never stored. The expression
refers to the current record through its record variable (see "Record variables in
table-scoped expressions") and may use functions and `$`-parameters, but not other
computed fields. Computed fields
cannot be written, and the first release does not index them.

### Types

| DDL type | Value | Status |
|---|---|---|
| `any` | Any value; disables the type check | Phase 2 |
| `bool`, `int`, `float`, `string` | `Bool`, `Integer(i64)`, `Float(f64)`, `String` | Phase 2 (values exist today) |
| `decimal` | 128-bit decimal | Phase 2 |
| `bytes` | Byte string | Phase 2 |
| `datetime`, `duration` | UTC nanosecond datetime; duration | Phase 2 (values exist today) |
| `uuid` | UUID | Phase 2 |
| `array<T>` | Array with typed elements; `array` alone means `array<any>` | Phase 2 (untyped arrays exist today) |
| `object` | Nested document (ordered map) | Phase 2 (values exist today) |
| `vector<f32, N>` | Fixed-dimension `f32` vector | Phase 2 (vectors exist today) |
| `record<Table>` | `RecordId` that must point into `Table`; auto-dereferenced on read | Phase 2 (untyped links exist today) |
| `point`, `geometry` | WGS84 point; any geometry. Geo functions and the spatial index are Phase 3. | Phase 2 (values exist today) |
| `option<T>` | `T`, `null`, or absent | Phase 2 |

The full `Value` definition is in [03 — Data Model](03-data-model.md).

### Record links

A `record<T>` field stores a `RecordId`. On write, the target table must match `T`.
On read, property access through the field loads the target record, so
`o.customer.name` works without a join or `FETCH`. Links are covered in depth in
[08 — Document & Relational Model](08-document-relational.md).

Whether a link must point at an existing record is a constraint choice, not a type
choice. The default is to allow dangling links, which keeps deletes cheap; a dangling
link dereferences to `null`.

## Constraints (Implemented for nodes; relationships Phase 2)

Constraints use the Neo4j 5 syntax. The pattern is a node pattern for node tables and a
relationship pattern for edge tables.

```
CREATE CONSTRAINT [IF NOT EXISTS] name
    FOR (v:Table) | FOR ()-[v:EDGE_TABLE]-()
    REQUIRE v.prop IS UNIQUE
          | (v.prop, v.prop) IS UNIQUE
          | v.prop IS NOT NULL

DROP CONSTRAINT [IF EXISTS] name
```

```cypher
CREATE CONSTRAINT person_email_unique FOR (p:Person) REQUIRE p.email IS UNIQUE;
CREATE CONSTRAINT sku_per_store FOR (s:Stock) REQUIRE (s.store, s.sku) IS UNIQUE;
CREATE CONSTRAINT employment_role FOR ()-[w:WORKS_AT]-() REQUIRE w.role IS NOT NULL;
```

- A uniqueness constraint creates and owns a unique index with the same name. The
  index serves lookups (`NodeUniqueIndexSeek` in `EXPLAIN`).
- Uniqueness ignores nodes where a constrained property is missing or `null`; a
  `NODE KEY` requires every property and makes the combination unique. `1` and `1.0`
  count as the same value.
- A unique value that cannot be compared exactly (an object, vector, geometry or NaN)
  is rejected with `ConstraintViolation` rather than silently left unchecked.
- Two transactions claiming the same value write the same key, so the second to commit
  fails with `TxConflict` at every isolation level; a retried `MERGE` then finds the
  first node.
- Constraints are checked as each node is written, so swapping unique values between
  two nodes needs an intermediate value.
- `IS NOT NULL` is equivalent to a non-`option` field and is accepted for Neo4j
  compatibility on schemaless tables.
- Type constraints are expressed with `CREATE FIELD ... TYPE`, not with Neo4j's
  `IS :: TYPE` form.
- Creating a constraint validates existing records first and fails, with nothing
  changed, if any record violates it.

## Indexes (Range, composite, unique and vector implemented; others Planned — Phase 2 and 3)

All index kinds share the Neo4j 5 shape `CREATE <KIND> INDEX name FOR pattern ON ...`.
This chapter lists them; the owner chapter defines options, query use, and tuning.

| Kind | Example | Owner | Status |
|---|---|---|---|
| Range (B-tree) | `CREATE INDEX person_email FOR (p:Person) ON (p.email);` | [08](08-document-relational.md) | Implemented |
| Composite | `CREATE INDEX person_name FOR (p:Person) ON (p.last, p.first);` | [08](08-document-relational.md) | Implemented |
| Edge property | `CREATE INDEX works_since FOR ()-[w:WORKS_AT]-() ON (w.since);` | [09](09-graph-model.md) | Phase 2 |
| Unique | Created by `REQUIRE ... IS UNIQUE` or `IS NODE KEY` | this chapter | Implemented |
| Vector (HNSW) | `CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding) OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};` | [10](10-vector-index.md) | Implemented |
| Full-text | `CREATE FULLTEXT INDEX doc_text FOR (d:Doc) ON EACH [d.title, d.body] OPTIONS {analyzer: 'english'};` | [11](11-full-text-search.md) | Phase 3 |
| Point (spatial) | `CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);` | [12](12-geospatial.md) | Phase 3 |

```
DROP INDEX [IF EXISTS] name
```

Vector indexes are derived in memory and built on first search
([10 — Vector Index](10-vector-index.md)). Range, composite and unique indexes are
stored: their entries are written in the same transaction as the record, under the
`/px/` and `/ux/` key prefixes ([04 — Storage Engine](04-storage-engine.md)), and a new
index is built synchronously by the statement that creates it. Creating an index never
changes query results, only speed: every candidate an index returns is re-checked
against the full `WHERE`. Background builds for very large labels (a `POPULATING`
state) are planned.

## Analyzers (Planned — Phase 3)

An analyzer turns text into terms for full-text indexes.

```
CREATE ANALYZER [IF NOT EXISTS] name TOKENIZER tokenizer FILTERS [filter, ...]
DROP ANALYZER [IF EXISTS] name
```

```cypher
CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];
```

The available tokenizers and filters, and how analyzers combine with BM25 scoring, are
in [11 — Full-Text Search](11-full-text-search.md).

## Permissions Clause (Planned — Phase 5)

`PERMISSIONS` attaches record-level rules to a table, either in `CREATE TABLE` or with
`ALTER TABLE`, and field-level rules to a field in `CREATE FIELD`:

```
permission-rules = { FOR action [, action]... ( FULL | NONE | WHERE expression ) }...
action           = read | create | update | delete
```

```cypher
ALTER TABLE Post PERMISSIONS
  FOR read WHERE p.published OR p.author = $auth.id
  FOR create, update, delete WHERE p.author = $auth.id;

CREATE FIELD salary ON Employee TYPE decimal
  PERMISSIONS FOR read WHERE e.id = $auth.id FOR update NONE;
```

- The rule expression binds the current record as described in "Record variables in
  table-scoped expressions".
- A table with no `PERMISSIONS` clause gives `NONE` to application users (record
  access and JWT users). System users created with `CREATE USER` bypass permissions and
  are governed by their roles.

Users, roles, access methods, and how rules are evaluated are owned by
[18 — Security & Auth](18-security-auth.md).

## Change Feed Clause (Planned — Phase 4)

`CHANGEFEED duration` keeps an ordered log of every change to the table for the given
retention, readable with `CALL db.changes(...)`. Live queries and events are related
but do not require a change feed. See
[14 — Live Queries & Events](14-live-queries-events.md).

## Introspection (`SHOW INDEXES` and `SHOW CONSTRAINTS` implemented; the rest Planned — Phase 2)

Introspection uses Neo4j's `SHOW` form. Each `SHOW` statement returns ordinary rows.
`YIELD`, `WHERE` and `RETURN` after `SHOW` are planned.

| Statement | Rows describe |
|---|---|
| `SHOW NAMESPACES` | Namespaces visible to the user |
| `SHOW DATABASES` | Databases in the current namespace |
| `SHOW TABLES` | Tables in the current database: name, kind, mode, change feed |
| `SHOW TABLE Person` | One row with the full definition of a table, as DDL text |
| `SHOW FIELDS ON Person` | Declared fields: path, type, default, assertion, flags |
| `SHOW INDEXES` | Implemented: `name`, `type` (`RANGE`, `VECTOR`), `labelsOrTypes`, `properties`, `owningConstraint`. Planned: `state` |
| `SHOW CONSTRAINTS` | Implemented: `name`, `type` (`UNIQUENESS`, `NODE_PROPERTY_EXISTENCE`, `NODE_KEY`), `labelsOrTypes`, `properties` |
| `SHOW ANALYZERS` | Analyzers and their pipelines |

```cypher
SHOW INDEXES YIELD name, kind, state WHERE state <> 'ONLINE' RETURN name, kind, state;
```

`SHOW TABLE` output is valid DDL, so a schema can be exported by running it for every
table. The `atlantis export` command does this for whole databases
([17 — Server & Protocols](17-server-protocols.md)).

## Schema Migration Guidance

DDL is transactional per statement: each DDL statement either applies fully or not at
all. Index population is the exception, because it continues in the background after
`CREATE INDEX` commits. Recommended practice:

1. **Keep DDL in version control** as `.cypher` scripts and use `IF NOT EXISTS` /
   `IF EXISTS` so scripts can run repeatedly.
2. **Add before you require.** Add a new field as `option<T>`, backfill it in batches,
   then tighten it to `T`:

   ```cypher
   CREATE FIELD tier ON Customer TYPE option<string>;

   MATCH (c:Customer) WHERE c.tier IS NULL
   WITH c LIMIT 10000
   SET c.tier = 'standard';

   ALTER FIELD tier ON Customer TYPE string;
   ```

   Repeat the batch until it updates no rows. `ALTER FIELD` validates every existing
   record and fails, with nothing changed, if one does not fit.
3. **Switch to `SCHEMAFULL` last.** `ALTER TABLE ... SCHEMAFULL` validates existing
   records the same way. Declare every field first, or remove stray fields with
   `REMOVE`.
4. **Create indexes before the queries that need them**, and wait for `ONLINE` in
   `SHOW INDEXES` before relying on their performance.
5. **Rename by copy.** There is no rename statement. Create the new field, copy values
   with `SET`, move readers, then `DROP FIELD` the old one.

## Design Decisions

**Why tables under Cypher labels?**
A table gives each record one home for schema, permissions, indexes, and storage
layout, which documents and relations need. Mapping the first label to the table keeps
plain Cypher working unchanged. See [03 — Data Model](03-data-model.md).

**Why schemaless by default?**
Graph and AI workloads usually start with evolving data, and existing Cypher code
expects implicit labels. Schema is added per table when the shape stabilises, without a
data migration.

**Why Neo4j forms for indexes and constraints?**
They already exist in Cypher, and users and tools know them. Only concepts without a
Cypher equivalent (tables, fields, analyzers, change feeds, permissions) get new
statements.

**Why `SHOW` instead of a separate info command?**
`SHOW` results are rows, so they compose with `WHERE`, `YIELD`, and client code that
already reads query results.

## What's Next

- **[08 — Document & Relational Model](08-document-relational.md)** — Using fields, links, and indexes.
- **[09 — Graph Model](09-graph-model.md)** — Edge tables and traversal.
- **[05 — Transactions & Consistency](05-transactions-consistency.md)** — How DDL and data changes commit.
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — When each part ships.
