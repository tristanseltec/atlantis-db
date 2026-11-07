# 01 — Getting Started

> **Status:** Partially implemented — the embedded Rust quickstart, Cypher, transactions and vector search reflect today's API; the server quickstart and the other multi-model examples are Planned (phase marked per section)
> **Crates:** `atlantis-db`, `atlantis-server` (planned)

This guide shows how to use Atlantis DB as an embedded Rust library today, how the
standalone server will be started, and what multi-model queries in Atlantis Cypher will
look like. Each planned part is marked with the phase that delivers it (see
[Implementation Roadmap](25-implementation-roadmap.md)).

## What Works Today

Atlantis DB is pre-release. The embedded engine stores documents, graph nodes and edges
as records in namespaced tables. Cypher and the Rust record API read and write the same
records, in ACID transactions, and every commit is durable through a write-ahead log.
The authoritative list of working behaviour is the integration test suite in
`crates/atlantis-db/tests/`.

| Capability | Status |
|---|---|
| Embedded engine, in memory or in a directory (write-ahead log, periodic checkpoints) | Implemented |
| Cypher reads and writes: `MATCH` (multi-hop, any direction, variable length), `OPTIONAL MATCH`, `CREATE`, `MERGE`, `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`, `WITH`, `UNWIND`, `UNION` | Implemented |
| Expressions, aggregation, `DISTINCT`, `ORDER BY`, `SKIP`, `LIMIT`, parameters (`$name`) | Implemented |
| Scripts separated by `;`, `BEGIN` / `COMMIT` / `ROLLBACK` | Implemented |
| Rust record transactions (`engine.transaction`, `read`, retry on conflict), with Cypher inside them | Implemented |
| Nested documents and record links that dereference on property access | Implemented |
| `CREATE VECTOR INDEX`, `DROP INDEX`, `db.vector.search` | Implemented |
| RocksDB backend, connection URLs | Phase 1 |
| Schema DDL, secondary indexes, constraints, `USE` | Phase 2 |
| `shortestPath`, graph algorithms, full-text, geospatial indexes, key-value API, time series | Phase 3 |
| Live queries and events | Phase 4 |
| Server, HTTP, WebSocket RPC, auth | Phase 5 |
| SDKs for Python, JavaScript/TypeScript, Go, Java | Phase 6 |

## Embedded Quickstart (Rust)

### Prerequisites

- Rust 1.89 or newer (the workspace's `rust-version`), 2021 edition.
- A tokio runtime.

### Add the dependency

Atlantis DB is not published to crates.io yet. Depend on the facade crate from a local
checkout:

```toml
[dependencies]
atlantis-db = { path = "../atlantis-db/crates/atlantis-db" }
tokio = { version = "1", features = ["full"] }
```

The examples in this section compile against the current API.

### Open a database

```rust
use atlantis_db::Engine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Opens or creates a database in this directory and recovers every
    // committed transaction from its checkpoint and write-ahead log.
    let engine = Engine::open("./my_atlantis_db").await?;

    // ... run queries ...

    // Folds the write-ahead log into the checkpoint file. Commits are already
    // durable without this; it only shortens the next recovery.
    engine.shutdown().await?;
    Ok(())
}
```

For tests and throwaway data, open an in-memory engine:

```rust
let engine = Engine::open_in_memory().await?;
```

`Engine` is cheap to clone and safe to share across tasks. A directory can be open in
only one engine at a time (a `LOCK` file guards it). The database closes when the last
clone is dropped.

### Custom configuration

```rust
use atlantis_db::{Config, Engine};
use atlantis_db::config::DistanceMetricKind;

let config = Config::builder()
    .disk_path("./my_db")
    .vector_dimensions(384)
    .distance_metric(DistanceMetricKind::Cosine)
    .build()?;          // validates the values

let engine = Engine::open_with_config(config).await?;
```

All options and defaults are in [Configuration](22-configuration.md).

### Create and query nodes

```rust
use atlantis_db::Value;

engine.execute("CREATE (p:Person {name: 'Alice', age: 34})", &[]).await?;
engine.execute("CREATE (p:Person {name: 'Bob', age: 29})", &[]).await?;

let result = engine.execute(
    "MATCH (p:Person)
     WHERE p.age > $min_age
     RETURN p.name AS name, p.age AS age
     ORDER BY age DESC",
    &[("min_age", Value::from(30i64))],
).await?;

for row in result.rows() {
    println!("{} ({})", row.get_string("name").unwrap_or(""), row.get_int("age").unwrap_or(0));
}
```

- Parameters are `(&str, Value)` pairs referenced as `$name`.
- `Row` has typed accessors (`get_string`, `get_int`, `get_float`, `get_bool`,
  `get_vector`) that return `Option`, plus `get` for the raw `Value`.
- A node or relationship returned on its own (`RETURN p`) comes back as its document,
  with an `id` field holding its `RecordId`. `p.id` and `id(p)` return the `RecordId`.
  See [Data Model](03-data-model.md).

### Relationships, traversal and updates

```cypher
CREATE (a:Person {id: 'alice', name: 'Alice'})-[:KNOWS {since: 2020}]->(b:Person {id: 'bob', name: 'Bob'});

MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(friend:Person)
RETURN friend.name AS friend;

MATCH (p:Person {name: 'Alice'}) SET p.age = 35, p.address.city = 'Hue';

MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..3]->(b:Person)
RETURN DISTINCT b.name;
```

The `id` property sets the record key: the first statement creates the records
`Person:alice` and `Person:bob`. A node's first label is its table.

### Records from Rust

The same records are reachable from the Rust record API, and Cypher runs inside a Rust
transaction with `tx.execute`, seeing that transaction's uncommitted writes:

```rust
use atlantis_db::{Engine, Object, Value};

engine.transaction(|tx| {
    let mut carol = Object::new();
    carol.insert("id".into(), Value::from("carol"));       // key: Person:carol
    carol.insert("name".into(), Value::from("Carol"));
    let carol = tx.create_node(&["Person"], carol)?;
    tx.relate(&"Person:bob".parse()?, "KNOWS", &carol.id, Object::new())?;
    let rs = tx.execute("MATCH (:Person {id: 'bob'})-[:KNOWS]->(c) RETURN c.name AS name", &[])?;
    assert_eq!(rs.row_count(), 1);
    Ok(())
}).await?;
```

See [Transactions & Consistency](05-transactions-consistency.md) and
[API Reference](21-api-reference.md).

### Vector search

Create a vector index for a label and property, store vectors as `Vector` values (or
lists of numbers), and search with `db.vector.search`:

```rust
engine.execute(
    "CREATE VECTOR INDEX embedding_vec FOR (e:Embedding) ON (e.vector)
     OPTIONS {dimension: 384, metric: 'cosine'}",
    &[],
).await?;

let embedding: Vec<f32> = vec![0.1; 384];
engine.execute(
    "CREATE (e:Embedding {vector: $vec, text: 'Rust ownership'})",
    &[("vec", Value::from(embedding.clone()))],
).await?;

let hits = engine.execute(
    "CALL db.vector.search('embedding_vec', $q, 10) YIELD node, score
     RETURN node.text AS text, score",
    &[("q", Value::from(embedding))],
).await?;
```

`node` is the record id of the hit, so `node.text` reads its property. The full syntax
is in [Vector Index](10-vector-index.md).

## Server Quickstart (Planned — Phase 5)

The `atlantis` binary runs the same engine as a standalone server.

```bash
# Start a single node on the default port 7700, persisting to ./data
atlantis start --bind 0.0.0.0:7700 --user root --pass "$ATLANTIS_PASS" rocksdb://data

# Check that it is ready
atlantis isready
curl http://localhost:7700/health
```

Run a query over HTTP. Obtain a token with `POST /signin` first; the request and
response shapes are in [Server & Protocols](17-server-protocols.md).

```bash
curl -X POST http://localhost:7700/query \
  -H "Authorization: Bearer $TOKEN" \
  -H "Atlantis-NS: acme" \
  -H "Atlantis-DB: shop" \
  -H "Content-Type: application/json" \
  -d '{"query": "MATCH (p:Person) RETURN p.name AS name", "params": {}}'
```

For interactive use, `atlantis shell` opens a Cypher shell against a server. SDKs
connect with `ws://localhost:7700` (Phase 6, see [Client SDKs](19-client-sdks.md)).
The same Rust `Engine` API will accept `mem://`, `rocksdb://path`, `ws://`, `http://`
and `tikv://` connection URLs.

## A Taste of Multi-Model Atlantis Cypher

Every example below is plain Atlantis Cypher, sent through `engine.execute` embedded or
`POST /query` on a server (Phase 5). Each block says whether it works today or names its
phase.

### Namespaces, tables and schema (Phase 2)

```cypher
CREATE NAMESPACE acme;
CREATE DATABASE acme.shop;
USE acme.shop;

CREATE TABLE Person SCHEMAFULL;
CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';
CREATE FIELD age ON Person TYPE option<int> DEFAULT 0 ASSERT value >= 0;
CREATE CONSTRAINT person_email_unique FOR (p:Person) REQUIRE p.email IS UNIQUE;
```

See [Schema & DDL](07-schema-ddl.md).

### Documents and record links (implemented)

```cypher
CREATE (o:Order {
  customer: rid('Customer', 'c-17'),
  status: 'open',
  address: {city: 'Hue', street: '12 Le Loi'},
  lines: [{sku: 'A-1', qty: 2}]
});

// o.customer is a record link: property access follows it automatically
MATCH (o:Order) WHERE o.address.city = 'Hue'
RETURN o.customer.name AS customer, size(o.lines) AS lineCount;
```

See [Document & Relational](08-document-relational.md).

### Graph traversal (implemented; algorithms Phase 3)

```cypher
MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..3]->(b:Person)      // implemented
RETURN DISTINCT b.name;

CALL algo.pagerank('Person', 'KNOWS') YIELD node, score          // Phase 3
RETURN node.name, score ORDER BY score DESC LIMIT 10;
```

See [Graph Model](09-graph-model.md).

### Hybrid text + vector search (Phase 3; `CREATE VECTOR INDEX` implemented)

```cypher
CREATE FULLTEXT INDEX doc_text FOR (d:Doc) ON EACH [d.title, d.body]
  OPTIONS {analyzer: 'english'};
CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};

CALL db.search.hybrid({text: 'doc_text', vector: 'chunk_embedding'},
                      'graph database', $vec, 10) YIELD node, score
RETURN node.title, score;
```

See [Full-Text Search](11-full-text-search.md) and [RAG Patterns](24-rag-patterns.md).

### Geospatial (Phase 3; `point()` and `point.distance` work today)

```cypher
CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);

MATCH (p:Place)
WHERE point.distance(p.location, point({longitude: 107.59, latitude: 16.46})) < 2000
RETURN p.name;
```

See [Geospatial](12-geospatial.md).

### Key-value and time series (Phase 3)

```cypher
CALL db.kv.set('session:42', {user: 'alice'}, {ttl: '1h'});
CALL db.kv.get('session:42');

CREATE TABLE Reading TYPE TIMESERIES KEY (sensor, ts) RETENTION 30d;
MATCH (r:Reading) WHERE r.sensor = 'sensor-1' AND r.ts > time.now() - duration('PT1H')
RETURN ts.bucket(r.ts, duration('PT5M')) AS bucket, avg(r.value) AS avgValue;
```

See [Key-Value & Time-Series](13-key-value-time-series.md).

### Transactions (implemented)

```cypher
BEGIN;
MATCH (a:Account {id: 'a'}) SET a.balance = a.balance - 100;
MATCH (b:Account {id: 'b'}) SET b.balance = b.balance + 100;
COMMIT;
```

In Rust the same unit of work is written as `engine.transaction(|tx| { ... }).await`
with a synchronous closure, which does not retry on its own;
`engine.transaction_with_retry(max_attempts, f)` retries on `Error::TxConflict`. See
[Transactions & Consistency](05-transactions-consistency.md).

### Live queries and events (Phase 4)

```cypher
LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o;   // returns a live id
KILL LIVE $liveId;
```

See [Live Queries & Events](14-live-queries-events.md).

## What's Next

- [Architecture](02-architecture.md) — layers, crates and deployment modes.
- [Data Model](03-data-model.md) — records, RecordIds and value types.
- [Query Language](06-query-language.md) — the supported Cypher subset and extensions.
- [API Reference](21-api-reference.md) — the Rust API.
- [Query Cookbook](23-query-cookbook.md) — recipes per model.
- [RAG Patterns](24-rag-patterns.md) — retrieval and agent memory.
