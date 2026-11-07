# 23 — Query Cookbook

> **Status:** Partially implemented — a small graph + vector subset works today; most recipes are Planned — Phases 0–5
> **Crates:** `atlantis-cypher`, `atlantis-engine`, `atlantis-db`

Short, copyable Atlantis Cypher recipes grouped by data model. Each recipe carries a
status:

- **Works** — produced the expected result when run against the current executor.
- **Phase N** — the syntax is the canonical target. It parses or runs correctly only
  after that phase.

The grammar for each construct is owned by its chapter. This cookbook links to it
instead of repeating it.

## Implemented today

The "Works" results below come from running each query through
`atlantis_engine::QueryExecutor` on an in-memory `GraphStore`. The workspace needs the
Phase 0 build fixes first (see [21 — API Reference](21-api-reference.md#known-build-defects-phase-0)),
so these recipes cannot yet run through `Engine::execute`.

### Recipes that work

```cypher
// Create one node (literal or parameter properties). RETURN n gives its integer id.
CREATE (n:Person {name: 'Alice', age: 30}) RETURN n
CREATE (n:Person {name: $name, age: $age}) RETURN n

// Scan all nodes, or nodes with a label.
MATCH (n) RETURN n
MATCH (n:Person) RETURN n.name, n.age

// Filter with =, <, >, <=, >=, AND, OR, NOT. (`<>` does not parse yet; `!=` does.)
MATCH (n:Person) WHERE n.name = 'Alice' RETURN n.name, n.age
MATCH (n:Person) WHERE n.age >= 18 AND n.age <= 35 RETURN n.name AS name ORDER BY name DESC
MATCH (n:Person) WHERE n.name = $name AND n.age > $minAge RETURN n.name

// Order, skip, limit. ORDER BY must name a returned alias. SKIP must come before
// LIMIT; both take an integer literal or parameter.
MATCH (n:Person) RETURN n.name AS name ORDER BY name SKIP 20 LIMIT $pageSize

// WITH projection and a trailing WHERE.
MATCH (n:Person) WITH n.name AS name RETURN name
MATCH (n:Person) WITH n WHERE n.age > 26 RETURN n.name

// Arithmetic on matching types and string concatenation.
MATCH (n:Person) RETURN n.age + 1 AS nextAge, n.name + '!' AS shout

// Delete filtered nodes. The node is tombstoned, not physically removed.
MATCH (n:Person) WHERE n.name = 'Alice' DELETE n
```

```rust
let rows = engine.execute(
    "MATCH (n:Person) WHERE n.name = $name RETURN n.age AS age",
    &[("name", "Alice".into())],
).await?;
```

### Vector search (does not work today)

```cypher
CALL db.vector.search('Embedding', $queryVec, 10) YIELD node, score RETURN node, score
CALL db.vector.search('Embedding', $queryVec, 10, 0.7) YIELD node, score RETURN node, score
```

The procedure parses and runs, but returns no rows through the engine:

- The first argument is a label, and the executor ignores it. There is one global HNSW
  index. From Phase 3 the first argument is a vector index name
  ([10 — Vector Index](10-vector-index.md)).
- The engine never populates the HNSW index. Creating a node with a `Vector` property
  does not index it, so the search always returns no rows. Phase 0 wires indexing.
- `k` and the optional threshold must be literals. `$k` raises `InvalidQuery`.
  `score` is `1 - distance`, and the first `YIELD` column holds an internal integer id,
  not a node.

### Recipes that do not work yet

| Recipe | Today | Fixed in |
|---|---|---|
| `CALL db.vector.search(...)` and every vector + graph recipe | Returns no rows: the HNSW index is never populated. | Phase 0 |
| `CREATE (a)-[:KNOWS]->(b)` | Creates both nodes, drops the relationship, and returns only the last node. | Phase 0 |
| `MATCH (a)-[:KNOWS]->(b) RETURN a.name, b.name` | Multi-hop binding is broken: returns every `b` with `a` unbound. | Phase 0 |
| `MATCH (a)<-[:KNOWS]-(b)`, `(a)--(b)`, `(a)-[]-(b)` | Parse errors. `Expand` also follows outgoing edges only, and an untyped relationship returns no rows. | Phase 0 |
| `MATCH (a:A) MATCH (b:B)` | Only the last scan's rows are kept. | Phase 0 |
| `MATCH (n:Person {name: 'Alice'})` | The inline property map is ignored, so every `:Person` matches. Never combine it with `DELETE` today. Use `WHERE`. | Phase 0 |
| `MATCH ... SET n.age = 31` | Parse error (`expected Equals`), and the planner would build `SetProperty` on an empty input, so nothing is updated. | Phase 0 |
| `CREATE (a:X), (b:Y)` | Parse error on the comma. Use two statements. | Phase 0 |
| `UNWIND [1, 2, 3] AS x RETURN x`, `RETURN 1` | Returns no rows when `UNWIND` or `RETURN` is the first clause. | Phase 0 |
| `MATCH ... CREATE ...` | `CREATE` ignores the matched rows. | Phase 0 |
| `DETACH DELETE n` | Deletes the node and leaves its edges. | Phase 0 |
| `-[:KNOWS*1..3]->` | Parse error (`invalid float: 1..3`). | Phase 0 |
| `MERGE`, `REMOVE` | Not executed: rejected by the parser today and ignored by the planner. | Phase 0 |
| `count()`, `collect()`, `id()` and other functions | Evaluate to `null`. | Phase 0 |
| `RETURN DISTINCT`, `WITH ... ORDER BY / LIMIT` | Parsed, then ignored. | Phase 0 |
| `ORDER BY n.name` when only an alias is returned, `ORDER BY ... ASC`, `LIMIT` before `SKIP`, `CASE`, `n.a.b` | Everything the `RETURN` parser does not recognise is silently dropped, and `ORDER BY` only resolves returned aliases. | Phase 0 |
| `OPTIONAL MATCH` | Behaves as `MATCH`, with no null row. | Phase 0 |
| `... UNION ...` | Everything after the first `RETURN` is ignored. | Phase 0 |
| `<>`, `IN`, `IS NULL`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `XOR`, `count(*)` | Parse error. | Phase 0 |
| `%` | Evaluates to `null`. | Phase 0 |
| Map literal `{a: 1}` in an expression | Evaluates to `null`. | Phase 2 (`Object`) |
| `db.vector.similarity(a, b)` in `WHERE` | Parse error: dotted function names only parse after `CALL`. | Phase 0 |

## Graph (Phase 0 for core; Phase 3 for paths and algorithms)

Grammar: [09 — Graph Model](09-graph-model.md).

```cypher
// Create a relationship between existing nodes.            Phase 0
MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'})
CREATE (a)-[:KNOWS {since: 2020}]->(b)

// Upsert.                                                   Phase 0
MERGE (p:Person {email: $email})
  ON CREATE SET p.created = time.now()
  ON MATCH SET p.seen = time.now()

// Friends of friends, 1–3 hops, deduplicated.               Phase 0
MATCH (me:Person {name: $name})-[:KNOWS*1..3]->(f:Person)
WHERE f <> me
RETURN DISTINCT f.name

// Shortest path and path functions.                         Phase 3
MATCH p = shortestPath((a:Person {name: 'Alice'})-[:KNOWS*]-(b:Person {name: 'Eve'}))
RETURN length(p), [n IN nodes(p) | n.name] AS hops

// Degree with aggregation.                                  Phase 0
MATCH (p:Person)-[:KNOWS]->(f)
RETURN p.name, count(f) AS degree ORDER BY degree DESC LIMIT 10

// Graph algorithms.                                         Phase 3
CALL algo.pagerank({label: 'Page', relationship: 'LINKS'}) YIELD node, score
RETURN node.url, score ORDER BY score DESC LIMIT 20
```

## Document (Phase 2)

Grammar: [08 — Document and Relational](08-document-relational.md).

```cypher
CREATE (p:Person {name: 'An', address: {city: 'Hue', zip: '530000'}, tags: ['vip']})

MATCH (p:Person) WHERE p.address.city = 'Hue' RETURN p.name, p.address

MATCH (p:Person {name: 'An'})
SET p.address.city = 'Da Nang', p += {phone: '+84 90 000 0000'}

MATCH (p:Person) WHERE 'vip' IN p.tags RETURN p.name

// Address a record by id.
MATCH (p:Person) WHERE id(p) = rid('Person', 'an') RETURN p
```

## Relational: links, joins and aggregation (Phase 2)

Schema first ([07 — Schema and DDL](07-schema-ddl.md)):

```cypher
CREATE TABLE Customer SCHEMAFULL;
CREATE FIELD name ON Customer TYPE string;
CREATE TABLE Order SCHEMAFULL;
CREATE FIELD customer ON Order TYPE record<Customer>;
CREATE FIELD total ON Order TYPE decimal ASSERT value >= 0;
CREATE INDEX order_customer FOR (o:Order) ON (o.customer);
```

```cypher
// Record links dereference on property access. No join clause needed.
MATCH (o:Order) WHERE o.total > 100 RETURN o.id, o.customer.name

// Join on a link and aggregate per customer.
MATCH (o:Order)
RETURN o.customer.name AS customer, count(o) AS orders, sum(o.total) AS revenue
ORDER BY revenue DESC LIMIT 10

// Explicit value join between two tables.
MATCH (o:Order), (c:Customer) WHERE o.customerEmail = c.email
RETURN c.name, collect(o.total) AS totals

// Unique constraint.
CREATE CONSTRAINT customer_email_unique FOR (c:Customer) REQUIRE c.email IS UNIQUE;
```

## Full-text search (Phase 3)

Grammar: [11 — Full-Text Search](11-full-text-search.md).

```cypher
CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];
CREATE FULLTEXT INDEX doc_text FOR (d:Doc) ON EACH [d.title, d.body]
  OPTIONS {analyzer: 'english'};

CALL db.text.search('doc_text', 'graph database', 10) YIELD node, score
RETURN node.title, text.highlight(node.body, 'graph database') AS snippet, score
```

## Vector and hybrid search (Phase 3)

Grammar: [10 — Vector Index](10-vector-index.md), [11 — Full-Text Search](11-full-text-search.md).

```cypher
CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};

// Vector search, then filter and traverse.
CALL db.vector.search('chunk_embedding', $vec, 20) YIELD node, score
WHERE node.lang = 'en'
MATCH (node)<-[:HAS_CHUNK]-(d:Doc)
RETURN d.title, node.text, score ORDER BY score DESC LIMIT 5

// Hybrid text + vector with reciprocal rank fusion.
CALL db.search.hybrid({text: 'doc_text', vector: 'chunk_embedding'},
                      'graph database', $vec, 10) YIELD node, score
RETURN node.title, score
```

## Geospatial (Phase 3)

Grammar: [12 — Geospatial](12-geospatial.md).

```cypher
CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);
CREATE (:Place {name: 'Imperial City', location: point({longitude: 107.578, latitude: 16.469})})

// Places within 2 km, nearest first.
WITH point({longitude: 107.59, latitude: 16.46}) AS here
MATCH (p:Place) WHERE point.distance(p.location, here) < 2000
RETURN p.name, point.distance(p.location, here) AS metres ORDER BY metres

// Inside a polygon.
MATCH (p:Place) WHERE geo.within(p.location, geo.polygon($ring)) RETURN p.name
```

## Key-value (Phase 3)

Grammar: [13 — Key-Value and Time-Series](13-key-value-time-series.md).

```cypher
CALL db.kv.set('session:42', {user: 'alice'}, {ttl: '1h'})
CALL db.kv.get('session:42') YIELD value RETURN value
CALL db.kv.scan('session:') YIELD key, value RETURN key, value
CALL db.kv.delete('session:42')
```

```rust
engine.kv().set("session:42", value, Some(Duration::from_secs(3600))).await?;
```

## Time-series (Phase 3)

```cypher
CREATE TABLE Reading TYPE TIMESERIES KEY (sensor, ts) RETENTION 30d;

CREATE (:Reading {sensor: 'sensor-1', ts: time.now(), celsius: 21.4})

// Five-minute averages over the last day.
MATCH (r:Reading)
WHERE r.sensor = 'sensor-1' AND r.ts >= time.now() - duration('P1D')
RETURN ts.bucket(r.ts, duration('PT5M')) AS bucket, avg(r.celsius) AS avgC
ORDER BY bucket
```

## Live queries and events (Phase 4)

Grammar: [14 — Live Queries and Events](14-live-queries-events.md).

```cypher
CALL db.changes('Order', {since: $versionstamp}) YIELD change RETURN change

LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o;   // returns a live id
KILL LIVE $liveId;

ALTER TABLE Order CHANGEFEED 7d;

CREATE EVENT order_shipped ON Order
  WHEN $event = 'UPDATE' AND $before.status <> 'shipped' AND $after.status = 'shipped'
  THEN { CREATE (:Notification {order: $after.id, at: time.now()}) };
```

## Transactions (Phase 1)

Semantics: [05 — Transactions and Consistency](05-transactions-consistency.md).

```cypher
BEGIN;
MATCH (a:Account {id: $from}) SET a.balance = a.balance - $amount;
MATCH (a:Account {id: $to})   SET a.balance = a.balance + $amount;
COMMIT;
```

```rust
engine.transaction(|tx| async move {
    tx.execute("MATCH (a:Account {id: $from}) SET a.balance = a.balance - $amount", &p).await?;
    tx.execute("MATCH (a:Account {id: $to}) SET a.balance = a.balance + $amount", &p).await?;
    Ok(())
}).await?;   // Err(TxConflict) may be retried
```

## Tenancy and permissions (Phase 2 for tenancy; Phase 5 for security)

Grammar: [18 — Security and Auth](18-security-auth.md).

```cypher
CREATE NAMESPACE acme;
CREATE DATABASE acme.shop;
USE acme.shop;

CREATE USER analyst ON DATABASE PASSWORD $pw ROLES [viewer];

ALTER TABLE Post PERMISSIONS
  FOR read WHERE p.published OR p.author = $auth.id
  FOR create, update, delete WHERE p.author = $auth.id;

// After a record user signs in, this returns only permitted rows.
MATCH (p:Post) RETURN p.title
```

## Design Decisions

- **Status per recipe, not per chapter.** A reader can tell at a glance which line runs
  today. Each "Works" claim was produced by running the query against the current
  executor.
- **Canonical syntax only.** Planned recipes use the forms other chapters define, so
  copying a recipe never teaches a second dialect.

## What's Next

- [24 — RAG Patterns](24-rag-patterns.md) — recipes combined into retrieval pipelines.
- [06 — Query Language](06-query-language.md) — the full Atlantis Cypher catalogue.
- [25 — Implementation Roadmap](25-implementation-roadmap.md) — when each recipe lands.
