# 08 — Document & Relational Model

> **Status:** Partially implemented — nested documents, record links that dereference, joins by pattern, aggregation, `MERGE` and pagination work today; schema, typed links, secondary indexes, the join optimizer and map projections are Planned — Phase 2
> **Crates:** `atlantis-core`, `atlantis-cypher`, `atlantis-engine`, `atlantis-storage`, `atlantis-kv`, `atlantis-index` (planned)

Atlantis DB stores every record as a document: a map of fields whose values can be
scalars, arrays, nested objects, vectors, geometries, or links to other records. The
same records can be queried relationally, through typed fields, record links, joins,
grouping, and secondary indexes. This chapter shows how to model and query data that
way in Atlantis Cypher. Schema statements are defined in
[07 — Schema & DDL](07-schema-ddl.md); the record model itself is in
[03 — Data Model](03-data-model.md).

## Implemented Today

Records are documents, and Cypher reads and writes them directly. Verified against
`crates/atlantis-engine/src/exec/` and the integration tests in
`crates/atlantis-db/tests/cypher.rs`:

| Capability | Today |
|---|---|
| Property values | Every `Value` variant, including nested `Object` maps, arrays, `Vector`, `Datetime`, `Geometry` and `RecordId` links. |
| Nested access `n.a.b`, `list[i]`, `map['k']` | Works. A missing step yields `null`. |
| Nested writes | `SET n.a.b = v`, `SET n += {...}`, `SET n = {...}`, `REMOVE n.a.b` work. Writing into an array element (`n.list[0].x = v`) does not parse. |
| Record links | A stored `RecordId` dereferences on property access (`o.customer.name`). A dangling link gives `null`. |
| Filtering | All Cypher operators, including `IN`, `IS NULL`, string predicates and `=~`. |
| Joins | Comma patterns and several `MATCH` clauses join by value; `OPTIONAL MATCH` gives an outer join. Every join is a nested loop over label scans. |
| Aggregation | `count`, `count(*)`, `sum`, `avg`, `min`, `max`, `collect`, `stdev`, with `DISTINCT` and implicit grouping. |
| Upsert | `MERGE` with `ON CREATE` / `ON MATCH`; `MERGE (c:Customer {id: 'ana'})` is a key lookup. |
| Pagination | `ORDER BY`, `SKIP` and `LIMIT` on `RETURN` and `WITH`, in either order. |
| Indexes | Label index and record-key lookups only. Every property filter scans the label. |
| Not yet | Schema DDL and typed fields, `record<T>` types, secondary indexes, map projections (`p {.name}`), `percentileCont` / `percentileDisc`. |

Sections below marked Planned are target design.

## Documents (Implemented; schema Phase 2)

A record is an ordered map. Field values may be objects and arrays nested to any depth.

```cypher
CREATE (p:Person {
  id: 'ana',
  name: 'Ana',
  address: {street: '12 Le Loi', city: 'Hue', zip: '530000'},
  phones: [{kind: 'mobile', number: '+84 90 000 0000'}],
  tags: ['customer', 'beta']
});
```

`id: 'ana'` sets the record key, giving `Person:ana`. Without it a ULID key is
generated.

### Reading nested values

Dotted paths read into objects. A missing step yields `null`, never an error.

```cypher
MATCH (p:Person) WHERE p.address.city = 'Hue'
RETURN p.name, p.address.street, p.phones[0].number AS firstPhone;
```

The standard Cypher list and map tools work on nested data (the map projection
`p {...}` is not implemented and not scheduled yet; use a map literal):

```cypher
MATCH (p:Person)
RETURN {name: p.name, city: p.address.city} AS card,
       [ph IN p.phones WHERE ph.kind = 'mobile' | ph.number] AS mobiles,
       size(p.tags) AS tagCount,
       keys(p.address) AS addressFields;
```

### Writing nested values

| Operation | Syntax | Notes |
|---|---|---|
| Set a nested field | `SET p.address.city = 'Da Nang'` | Missing intermediate objects are created. |
| Merge fields into a record | `SET p += {nickname: 'A', address: {zip: '530000'}}` | Shallow: `address` is replaced as a whole. |
| Replace all fields | `SET p = {name: 'Ana'}` | Keeps the record key and table. |
| Remove a nested field | `REMOVE p.address.zip` | |
| Append to an array | `SET p.tags = p.tags + ['vip']` | |
| Remove array elements | `SET p.tags = [t IN p.tags WHERE t <> 'beta']` | |
| Update one array element | `SET p.phones[0].number = $num` | Planned — Phase 2. Today rebuild the list with a comprehension. |

Nested paths in `SET` and `REMOVE` are an Atlantis extension; Neo4j only allows
top-level properties.

### Schemaless or schemafull (Planned — Phase 2)

A table is schemaless until you declare otherwise. Choose per table:

| Choose | When |
|---|---|
| `SCHEMALESS`, no fields | Prototyping, event payloads, data from external sources whose shape you do not control |
| `SCHEMALESS` + some fields | Most application tables: enforce the fields you depend on, allow extras |
| `SCHEMAFULL` | Records that other systems trust: billing, identities, anything with permissions on fields |

```cypher
CREATE TABLE Customer SCHEMAFULL;
CREATE FIELD name ON Customer TYPE string;
CREATE FIELD email ON Customer TYPE string ASSERT value =~ '.+@.+';
CREATE FIELD address ON Customer TYPE option<object>;
CREATE FIELD address.city ON Customer TYPE option<string>;
```

## Record Links (Implemented; `record<T>` types Phase 2)

A field holding a `RecordId` points at another record. From Phase 2 a field of type
`record<T>` restricts it to table `T`.

```cypher
// Phase 2
CREATE TABLE Order SCHEMAFULL;
CREATE FIELD customer ON Order TYPE record<Customer>;
CREATE FIELD items ON Order TYPE array<object>;
CREATE FIELD items.*.product ON Order TYPE record<Product>;
CREATE FIELD total ON Order TYPE decimal;
```

Create a link from a matched record or from a known key:

```cypher
MATCH (c:Customer) WHERE c.email = $email
CREATE (:Order {customer: id(c), total: 0, items: []});

CREATE (:Order {customer: rid('Customer', 'ana'), total: 0, items: []});
```

### Auto-dereference

Property access through a link loads the target record. No `FETCH` or join is needed:

```cypher
MATCH (o:Order) WHERE o.customer.address.city = 'Hue'
RETURN id(o), o.customer.name, [i IN o.items | i.product.name] AS products;
```

- Returning the link field itself (`RETURN o.customer`) returns the `RecordId`, not the
  record. Embedding the whole target with `o.customer {.*}` needs map projections (not
  implemented); `properties(o.customer)` returns its fields today.
- A dangling link (the target was deleted) dereferences to `null`.
- Each dereference is a point lookup by key. Batching lookups per operator is planned
  with the optimizer ([15 — Query Planner & Executor](15-query-planner-executor.md)).
- Permissions (Phase 5) apply to the target table as well: a link the user cannot read
  dereferences to `null` ([18 — Security & Auth](18-security-auth.md)).

`items.*.product` in the field path above declares the type of a field inside every
array element.

## Links or Edges

Both connect records. Choose by how the relationship is used:

| Question | Record link (`record<T>` field) | Edge (edge table) |
|---|---|---|
| Direction of navigation | One way, from the owning record | Both ways, with equal cost |
| Properties on the relationship | No (put them on the owner) | Yes, edges are records |
| Cardinality | One link, or an array of links | Any |
| Query style | `o.customer.name` | `MATCH (o)-[:PLACED_BY]->(c)` |
| Typical use | Foreign keys, ownership, lookups | Social graphs, many-to-many with attributes, multi-hop traversal |
| Reverse lookup cost | Needs an index on the link field | Adjacency entry, no extra index |

### One-to-many

Put a link on the "many" side and index it, as in relational design:

```cypher
CREATE FIELD customer ON Order TYPE record<Customer>;
CREATE INDEX order_customer FOR (o:Order) ON (o.customer);

// all orders of one customer
MATCH (o:Order) WHERE o.customer = $customerId RETURN o ORDER BY o.createdAt DESC;
```

### Many-to-many

Use an array of links when the relationship has no attributes and one side is small:

```cypher
CREATE FIELD tags ON Post TYPE array<record<Tag>>;
MATCH (p:Post) WHERE rid('Tag', 'rust') IN p.tags RETURN p.title;
```

Use an edge table when the relationship carries data or must be traversed from both
ends:

```cypher
CREATE TABLE ENROLLED_IN TYPE EDGE FROM Student TO Course;
CREATE FIELD grade ON ENROLLED_IN TYPE option<string>;

MATCH (s:Student)-[e:ENROLLED_IN]->(c:Course {code: 'CS101'})
RETURN s.name, e.grade;
```

Edges are covered in [09 — Graph Model](09-graph-model.md).

## Joins (Implemented as nested loops; optimizer Phase 2)

A join is a pattern with several parts connected by a shared value. Cypher already
expresses this with comma-separated patterns or several `MATCH` clauses:

```cypher
MATCH (o:Order), (c:Customer)
WHERE o.customer = id(c) AND c.country = 'VN'
RETURN c.name, o.total;
```

```cypher
MATCH (s:Shipment)
MATCH (w:Warehouse) WHERE w.code = s.warehouseCode
RETURN s.id, w.city;
```

Today every join is a nested loop: each part scans its label for every row of the
previous part. In Phase 2 the cost-based optimizer chooses the join strategy from
statistics and available indexes: index nested-loop when the inner side has an index on the join key, hash join
for large unindexed inputs. Auto-dereference of a link is a join the planner already
knows the shape of. Join operators and index selection are described in
[15 — Query Planner & Executor](15-query-planner-executor.md).

An `OPTIONAL MATCH` gives a left outer join:

```cypher
MATCH (c:Customer)
OPTIONAL MATCH (o:Order) WHERE o.customer = id(c)
RETURN c.name, count(o) AS orders;
```

## Aggregation and Grouping (Implemented)

Aggregation follows Cypher: every non-aggregated `RETURN` or `WITH` item is a grouping
key. There is no `GROUP BY` clause.

| Function | Result |
|---|---|
| `count(*)`, `count(x)`, `count(DISTINCT x)` | Number of rows, or of non-null values |
| `sum(x)`, `avg(x)` | Sum and mean of numbers |
| `min(x)`, `max(x)` | Smallest and largest value |
| `collect(x)` | Array of non-null values |
| `stDev(x)` | Sample standard deviation |
| `percentileCont(x, p)`, `percentileDisc(x, p)` | Percentiles (planned — Phase 2) |

```cypher
MATCH (o:Order)
WHERE o.createdAt >= $from
RETURN o.customer.country AS country,
       count(*) AS orders,
       sum(o.total) AS revenue,
       avg(o.total) AS avgOrder
ORDER BY revenue DESC;
```

Filter on an aggregate with `WITH ... WHERE`, the Cypher equivalent of `HAVING`:

```cypher
MATCH (o:Order)
WITH o.customer AS customer, sum(o.total) AS spent
WHERE spent > 1000
RETURN customer.name, spent;
```

Time bucketing for time-series grouping uses `ts.bucket` and is described in
[13 — Key-Value & Time-Series](13-key-value-time-series.md).

## Secondary Indexes (Planned — Phase 2)

Without an index, a property filter scans every record of the table. `atlantis-index`
adds three kinds of secondary index, declared with the Neo4j 5 forms in
[07 — Schema & DDL](07-schema-ddl.md):

| Kind | DDL | Serves |
|---|---|---|
| Range | `CREATE INDEX person_email FOR (p:Person) ON (p.email);` | Equality, ranges (`<`, `>=`), `STARTS WITH`, `IN`, `ORDER BY` on the indexed field |
| Unique | `CREATE CONSTRAINT person_email_unique FOR (p:Person) REQUIRE p.email IS UNIQUE;` | Same as range, and rejects duplicates |
| Composite | `CREATE INDEX person_name FOR (p:Person) ON (p.last, p.first);` | Equality on a prefix of the fields, then a range on the next field |

Behaviour of indexes:

- Index entries are ordered, memcomparable keys under `.../ix/{index}/...`, written in
  the same transaction as the record, so an index is never stale for a committed
  read ([04 — Storage Engine](04-storage-engine.md)).
- Nested paths can be indexed: `ON (p.address.city)`. Indexing an array field adds one
  entry per element, so `$tag IN p.tags` can use the index.
- `null` and missing values are not indexed. `IS NULL` filters cannot use an index.
- A composite index on `(last, first)` serves `last = $l AND first = $f` and
  `last = $l AND first STARTS WITH $p`, but not a filter on `first` alone.
- The planner picks indexes on its own. Use `EXPLAIN` to see whether a query used
  `IndexSeek` or `IndexRangeScan` ([15 — Query Planner & Executor](15-query-planner-executor.md)).

## Pagination

`SKIP` and `LIMIT` work on `RETURN` and `WITH`, in either order. Offset pagination
reads and discards the skipped rows, so its cost grows with the offset:

```cypher
MATCH (p:Product) RETURN p.name AS name ORDER BY name SKIP $offset LIMIT 20
```

For deep pages, use keyset pagination with the record id as a tiebreaker. The query
works today; it becomes cheap once the field is indexed (Phase 2):

```cypher
MATCH (p:Product)
WHERE p.createdAt < $lastCreatedAt
   OR (p.createdAt = $lastCreatedAt AND id(p) < $lastId)
RETURN p ORDER BY p.createdAt DESC, id(p) DESC LIMIT 20;
```

With an index on `createdAt`, each page costs the same regardless of depth.

## Upsert with MERGE (Implemented; constraints Phase 2)

`MERGE` matches a pattern or creates it, with Cypher's `ON CREATE` and `ON MATCH`
actions:

```cypher
MERGE (c:Customer {email: $email})
ON CREATE SET c.name = $name, c.createdAt = time.now()
ON MATCH SET c.lastSeen = time.now()
RETURN c;
```

- A `null` property value in the `MERGE` pattern is an error.
- `MERGE` is only safe against concurrent duplicates when a uniqueness constraint
  (Phase 2) covers the merge key. Without one, two concurrent transactions can both create the
  record. With one, the second fails with a constraint error or a conflict and can be
  retried ([05 — Transactions & Consistency](05-transactions-consistency.md)).
- To upsert by record key, merge on `id`: `MERGE (c:Customer {id: 'ana'})`. This is a
  point lookup and needs no extra index.

## Relational Modelling Example (Planned — Phase 2)

A small shop schema with links, an edge table, indexes, and a report query:

```cypher
USE acme.shop;

CREATE TABLE Customer SCHEMAFULL;
CREATE FIELD name    ON Customer TYPE string;
CREATE FIELD email   ON Customer TYPE string ASSERT value =~ '.+@.+';
CREATE FIELD country ON Customer TYPE string;
CREATE CONSTRAINT customer_email_unique FOR (c:Customer) REQUIRE c.email IS UNIQUE;

CREATE TABLE Product SCHEMAFULL;
CREATE FIELD name  ON Product TYPE string;
CREATE FIELD price ON Product TYPE decimal ASSERT value >= 0;

CREATE TABLE Order SCHEMAFULL;
CREATE FIELD customer        ON Order TYPE record<Customer>;
CREATE FIELD createdAt       ON Order TYPE datetime DEFAULT time.now() READONLY;
CREATE FIELD items           ON Order TYPE array<object>;
CREATE FIELD items.*.product ON Order TYPE record<Product>;
CREATE FIELD items.*.qty     ON Order TYPE int ASSERT value > 0;
CREATE FIELD total           ON Order COMPUTED reduce(s = 0, i IN o.items | s + i.qty * i.product.price);
CREATE INDEX order_customer_created FOR (o:Order) ON (o.customer, o.createdAt);

CREATE TABLE ALSO_BOUGHT TYPE EDGE FROM Product TO Product;
CREATE FIELD times ON ALSO_BOUGHT TYPE int DEFAULT 1;
```

Top customers by country this month:

```cypher
MATCH (o:Order) WHERE o.createdAt >= $monthStart
WITH o.customer AS c, sum(o.total) AS spent
RETURN c.country AS country, c.name AS customer, spent
ORDER BY spent DESC LIMIT 10;
```

Recent orders of one customer, served by the composite index:

```cypher
MATCH (o:Order)
WHERE o.customer = $customerId AND o.createdAt >= $since
RETURN o {.createdAt, .total, items: [i IN o.items | {name: i.product.name, qty: i.qty}]}
ORDER BY o.createdAt DESC;
```

More recipes are in [23 — Query Cookbook](23-query-cookbook.md).

## Design Decisions

**Why links and edges both?**
Links are cheap foreign keys that read naturally (`o.customer.name`) and need no
traversal syntax. Edges carry properties and are navigable in both directions. Real
schemas need both, and each record lives in one place either way.

**Why auto-dereference instead of `FETCH`?**
Property access is already how Cypher reads values, so following a link should look
the same. The planner knows the link type from the schema and can batch lookups, so a
separate fetch clause would add syntax without adding control.

**Why no `GROUP BY`?**
Cypher's implicit grouping already covers it, and adding a second grouping form would
make the same query writable two ways with subtly different rules.

**Why are secondary indexes transactional?**
An index written in the same transaction as the record can never return a record that
no longer matches. This keeps unique constraints and `MERGE` correct without a
separate consistency mode.

## What's Next

- **[07 — Schema & DDL](07-schema-ddl.md)** — Full grammar for tables, fields, indexes, and constraints.
- **[09 — Graph Model](09-graph-model.md)** — Edges, traversal, and paths.
- **[13 — Key-Value & Time-Series](13-key-value-time-series.md)** — Composite keys and range scans.
- **[15 — Query Planner & Executor](15-query-planner-executor.md)** — Joins and index selection.
