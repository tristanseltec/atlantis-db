# 01 — Getting Started

> **Status:** Partially implemented — the embedded Rust quickstart reflects today's API; the server quickstart and multi-model examples are Planned (phase marked per section)
> **Crates:** `atlantis-db`, `atlantis-server` (planned)

This guide shows how to use Atlantis DB as an embedded Rust library today, how the
standalone server will be started, and what multi-model queries in Atlantis Cypher will
look like. Each planned part is marked with the phase that delivers it (see
[Implementation Roadmap](25-implementation-roadmap.md)).

## What Works Today

Atlantis DB is pre-release. The workspace builds, but the Cypher engine still has the
Phase 0 gaps in the table below. The record store (documents, nodes and edges in
namespaced tables, with transactions) works through the Rust API; Cypher cannot query
it yet.

| Capability | Status |
|---|---|
| Embedded engine, in memory or with a disk path | Implemented |
| `CREATE` single nodes with labels and properties | Implemented |
| Single-node `MATCH` by label with `WHERE`, `RETURN ... AS`, `ORDER BY`, `SKIP`, `LIMIT` | Implemented |
| Parameters (`$name`) | Implemented |
| Explicit checkpoint and checkpoint on shutdown; restore on open | Partial — restored nodes get new IDs (Phase 0) |
| `CREATE` with relationship patterns (relationships are skipped; only the last node is returned) | Phase 0 |
| Multi-hop `MATCH` (the far node is bound to every node with that label) | Phase 0 |
| `MATCH ... SET` (updates nothing today) | Phase 0 |
| `DELETE` (only tombstones the node) | Phase 0 |
| `RETURN` or `UNWIND` as the first clause (returns no rows) | Phase 0 |
| `MERGE`, `REMOVE`, `OPTIONAL MATCH` (silently ignored) | Phase 0 |
| Variable-length paths | Phase 0 |
| `db.vector.search` returning results (the HNSW index is never populated) | Phase 0 |
| Automatic checkpoints and eviction | Phase 0 |
| Transactions, RocksDB backend | Phase 1 |
| Tables, schema, documents, record links | Phase 2 |
| Full-text, geospatial, key-value, time series | Phase 3 |
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

The examples in this section compile; the queries they run are limited by the Phase 0
gaps listed above. The [Implementation Roadmap](25-implementation-roadmap.md) tracks
progress.

### Open a database

```rust
use atlantis_db::Engine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Opens or creates a database at this path.
    // If a checkpoint exists there, the nodes are restored from it
    // (with new node IDs until the Phase 0 fix).
    let engine = Engine::open("./my_atlantis_db").await?;

    // ... run queries ...

    // Writes a checkpoint (because a disk path is set) and stops the engine.
    engine.shutdown().await?;
    Ok(())
}
```

For tests and throwaway data, open an in-memory engine:

```rust
let engine = Engine::open_in_memory().await?;
```

`Engine` is cheap to clone and safe to share across tasks. Call `engine.checkpoint()`
to write a checkpoint without shutting down.

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
- A node returned on its own (`RETURN p`) comes back as its integer node ID today.
  It becomes a full record in Phase 2 (see [Data Model](03-data-model.md)).

### Relationships and updates (Phase 0)

The parser accepts relationship patterns in `CREATE`, but the planner skips the
relationship, so this statement creates only the two nodes today:

```cypher
CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})
```

Once the Phase 0 fixes for relationship creation and multi-hop binding land, traversal
and updates work as in any Cypher database:

```cypher
MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(friend:Person)
RETURN friend.name AS friend;

MATCH (p:Person {name: 'Alice'}) SET p.age = 35;   // updates nothing today
```

### Vector search (Planned — Phase 0)

Vector search returns nothing today, because the engine never inserts vectors into the
HNSW index. Vectors can still be stored as `Vector` property values. Write embedding
literals as `f32`:

```rust
let embedding: Vec<f32> = vec![0.12, 0.45, 0.78 /* ... 384 values */];
engine.execute(
    "CREATE (e:Embedding {vector: $vec, text: 'Rust ownership'})",
    &[("vec", Value::from(embedding))],
).await?;
```

Today's procedure call is parsed and planned, and runs against the empty index:

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7) YIELD node, score
RETURN node, score
```

Today's signature is `db.vector.search(label, vector, k[, threshold])`:

- The label is ignored.
- `k` and the optional threshold must be literals, not parameters.
- The first `YIELD` column holds the index's internal integer ID, not a node.
- `score` is `1 - distance`.

Phase 0 populates the index. From Phase 3 the signature becomes
`db.vector.search(indexName, vector, k[, thresholdOrOptions])`, where `indexName` is a
vector index created with `CREATE VECTOR INDEX`. The full design is in
[Vector Index](10-vector-index.md).

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

## A Taste of Multi-Model Atlantis Cypher (Planned)

Every example below is plain Atlantis Cypher, sent through `engine.execute` embedded or
`POST /query` on a server. Each block names its phase.

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

### Documents and record links (Phase 2)

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

### Graph traversal (Phase 0 and Phase 3)

```cypher
MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..3]->(b:Person)      // Phase 0
RETURN DISTINCT b.name;

CALL algo.pagerank('Person', 'KNOWS') YIELD node, score          // Phase 3
RETURN node.name, score ORDER BY score DESC LIMIT 10;
```

See [Graph Model](09-graph-model.md).

### Hybrid text + vector search (Phase 3)

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

### Geospatial (Phase 3)

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

### Transactions (Phase 1)

```cypher
BEGIN;
MATCH (a:Account {id: rid('Account', 'a')}) SET a.balance = a.balance - 100;
MATCH (b:Account {id: rid('Account', 'b')}) SET b.balance = b.balance + 100;
COMMIT;
```

In Rust the same unit of work is written as `engine.transaction(|tx| { ... }).await`
with a synchronous closure (implemented today for the record API), which does not retry
on its own;
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
