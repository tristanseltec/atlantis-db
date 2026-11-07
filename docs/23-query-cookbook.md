# 23 — Query Cookbook

> **Status:** Partially implemented — graph, document, link, aggregation, vector and transaction recipes work today; schema, search, key-value, time-series, reactive and security recipes are Planned — Phases 2–5
> **Crates:** `atlantis-cypher`, `atlantis-engine`, `atlantis-db`

Short, copyable Atlantis Cypher recipes grouped by data model. Each recipe carries a
status:

- **Works** — produced the expected result when run through `Engine::execute`.
- **Phase N** — the syntax is the canonical target. It parses or runs correctly only
  after that phase.

The integration tests in `crates/atlantis-db/tests/cypher.rs` are the authoritative list
of working behaviour.

The grammar for each construct is owned by its chapter. This cookbook links to it
instead of repeating it.

## Basics (Works)

```cypher
// Create nodes. `id` sets the record key (Person:alice); RETURN n gives the document.
CREATE (n:Person {id: 'alice', name: 'Alice', age: 30}) RETURN n
CREATE (n:Person {name: $name, age: $age}) RETURN n.id AS id

// Scan, filter, page.
MATCH (n:Person) WHERE n.age >= 18 AND n.name <> 'Bob' RETURN n.name AS name ORDER BY name DESC
MATCH (n:Person) RETURN n.name AS name ORDER BY name SKIP 20 LIMIT $pageSize

// Leading UNWIND / RETURN, list comprehension, CASE.
UNWIND [1, 2, 3] AS x RETURN x, CASE WHEN x % 2 = 0 THEN 'even' ELSE 'odd' END AS kind
RETURN [x IN range(1, 10) WHERE x % 3 = 0 | x * x] AS squares

// Several statements; each commits on its own.
CREATE (:Tag {name: 'a'}); CREATE (:Tag {name: 'b'});

// Introspection.
CALL db.labels() YIELD label RETURN label
```

```rust
let rows = engine.execute(
    "MATCH (n:Person) WHERE n.name = $name RETURN n.age AS age",
    &[("name", "Alice".into())],
).await?;
```

Not implemented and not scheduled yet: map projections (`n {.name}`), `EXISTS { }` and
`CALL { }` subqueries, `FOREACH`, `LOAD CSV`
([06 — Query Language](06-query-language.md)).

## Graph (Works; shortest paths and algorithms Phase 3)

Grammar: [09 — Graph Model](09-graph-model.md).

```cypher
// Create a relationship between existing nodes.            Works
MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'})
CREATE (a)-[:KNOWS {since: 2020}]->(b)

// Upsert.                                                   Works
MERGE (p:Person {email: $email})
  ON CREATE SET p.created = time.now()
  ON MATCH SET p.seen = time.now()

// Friends of friends, 1–3 hops, deduplicated.               Works
MATCH (me:Person {name: $name})-[:KNOWS*1..3]->(f:Person)
WHERE f <> me
RETURN DISTINCT f.name

// Shortest path and path functions.                         Phase 3
MATCH p = shortestPath((a:Person {name: 'Alice'})-[:KNOWS*]-(b:Person {name: 'Eve'}))
RETURN length(p), [n IN nodes(p) | n.name] AS hops

// Degree with aggregation.                                  Works
MATCH (p:Person)-[:KNOWS]->(f)
RETURN p.name, count(f) AS degree ORDER BY degree DESC LIMIT 10

// Remove a node and its relationships.                      Works
MATCH (p:Person {name: $name}) DETACH DELETE p

// Graph algorithms.                                         Phase 3
CALL algo.pagerank({label: 'Page', relationship: 'LINKS'}) YIELD node, score
RETURN node.url, score ORDER BY score DESC LIMIT 20
```

## Document (Works)

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

## Relational: links, joins and aggregation (queries work; schema Phase 2)

Schema first ([07 — Schema and DDL](07-schema-ddl.md)), Phase 2. The queries below
already work on schemaless tables, with `rid('Customer', 'c1')` stored as the link:

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

// Unique constraint.                                       Phase 2
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

## Vector and hybrid search (vector works; hybrid Phase 3)

Grammar: [10 — Vector Index](10-vector-index.md), [11 — Full-Text Search](11-full-text-search.md).

```cypher
CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};

// Vector search, then filter and traverse.                 Works
CALL db.vector.search('chunk_embedding', $vec, 20) YIELD node, score
WHERE node.lang = 'en'
MATCH (node)<-[:HAS_CHUNK]-(d:Doc)
RETURN d.title, node.text, score ORDER BY score DESC LIMIT 5

// Hybrid text + vector with reciprocal rank fusion.        Phase 3
CALL db.search.hybrid({text: 'doc_text', vector: 'chunk_embedding'},
                      'graph database', $vec, 10) YIELD node, score
RETURN node.title, score
```

## Geospatial (distance works; index and predicates Phase 3)

Grammar: [12 — Geospatial](12-geospatial.md).

```cypher
CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);    // Phase 3
CREATE (:Place {name: 'Imperial City', location: point({longitude: 107.578, latitude: 16.469})})

// Places within 2 km, nearest first. Works today as a scan.
WITH point({longitude: 107.59, latitude: 16.46}) AS here
MATCH (p:Place) WHERE point.distance(p.location, here) < 2000
RETURN p.name, point.distance(p.location, here) AS metres ORDER BY metres

// Inside a polygon.                                         Phase 3
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

## Transactions (Works)

Semantics: [05 — Transactions and Consistency](05-transactions-consistency.md).

```cypher
BEGIN;
MATCH (a:Account {id: $from}) SET a.balance = a.balance - $amount;
MATCH (a:Account {id: $to})   SET a.balance = a.balance + $amount;
COMMIT;
```

```rust
let p = [("from", Value::from("a")), ("to", Value::from("b")), ("amount", Value::from(100))];
engine.transaction(|tx| {
    tx.execute("MATCH (a:Account {id: $from}) SET a.balance = a.balance - $amount", &p)?;
    tx.execute("MATCH (a:Account {id: $to}) SET a.balance = a.balance + $amount", &p)?;
    Ok(())
}).await?;   // Err(TxConflict) may be retried
```

A failed statement inside the closure rolls the whole transaction back, even if the
closure handles the error.

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
  today. Each "Works" claim was produced by running the query through the engine.
- **Canonical syntax only.** Planned recipes use the forms other chapters define, so
  copying a recipe never teaches a second dialect.

## What's Next

- [24 — RAG Patterns](24-rag-patterns.md) — recipes combined into retrieval pipelines.
- [06 — Query Language](06-query-language.md) — the full Atlantis Cypher catalogue.
- [25 — Implementation Roadmap](25-implementation-roadmap.md) — when each recipe lands.
