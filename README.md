<div align="center">

# Atlantis DB

**A multi-model database for AI-native applications, built in Rust, queried with one language: Atlantis Cypher.**

</div>

---

Atlantis DB is being built as a multi-model database: graph, vector, document,
relational, key-value, time-series, full-text and geospatial data in one engine, with
live queries and events on top. Every model is stored as records in tables over one
transactional key-value layer, and every model is queried with **Atlantis Cypher**:
Cypher (following Neo4j 5 / openCypher / GQL syntax) plus extensions where Cypher has
none. The same engine is designed to run embedded in an application, as a standalone
server and as a distributed cluster.

> **Project status:** pre-release. The code today is an embedded engine in seven crates
> with one transactional record store (documents, graph nodes and edges in namespaced
> tables) on an in-memory MVCC key-value layer made durable by a write-ahead log. Cypher
> and the Rust record API read and write the same records. Everything not marked as
> implemented below is target design, marked with the phase that delivers it. See the
> [Implementation Roadmap](docs/25-implementation-roadmap.md).

Inspired by the best ideas from:

- **Neo4j** — the property graph model and Cypher
- **FalkorDB** — in-memory graph performance and simplicity
- **ClickHouse** — analytical throughput and columnar efficiency
- **SurrealDB** — the scope of a single multi-model, multi-deployment database

## Why Atlantis DB?

AI applications need relationships, embeddings, documents, text search and fresh data
at the same time. Stitching a graph database, a vector store, a document store and a
search engine together means sync jobs, duplicated permissions and several query
languages. Atlantis DB keeps all of it in one transactional store with one query
language, so a single query can find semantically similar chunks, rank them with
full-text relevance, follow graph relationships and filter on document fields.

## Capabilities

| Capability | Status |
|---|---|
| Embedded Rust library (`atlantis-db`) | Implemented (in-memory or a directory; one engine per directory) |
| Cypher on the record store: `MATCH` (multi-hop, any direction, variable length, paths), `OPTIONAL MATCH`, `CREATE`, `MERGE`, `SET`, `REMOVE`, `DELETE`, `WITH`, `UNWIND`, `UNION`, aggregation | Implemented |
| Transactional KV layer (`atlantis-kv`), MVCC, snapshot and serializable isolation, write-ahead log with sync modes `every` / `never` | Implemented (in-memory `mem` backend) |
| Transactions: Rust closures (`engine.transaction`, `read`, retry on conflict) with Cypher inside, and `BEGIN` / `COMMIT` scripts | Implemented |
| Record model: namespaces, databases, tables, `RecordId`, nested documents, record links, edge tables, label index, extended value types | Implemented (`Decimal`, `Range` Phase 2) |
| HNSW vector index: `CREATE VECTOR INDEX`, `db.vector.search` | Implemented (derived from records; filtering and quantization Phase 3) |
| RocksDB backend, connection URLs, commit sequencer, query timeouts | Phase 1 |
| Schema DDL, secondary indexes, constraints, cost-based optimizer | Phase 2 |
| Full-text search, geospatial indexes, key-value API, time-series tables, shortest paths, graph algorithms | Phase 3 |
| Change feeds, events, live queries | Phase 4 |
| `atlantis` server, HTTP, WebSocket RPC, users, roles, permissions | Phase 5 |
| SDKs (Rust remote, Python, JavaScript/TypeScript + WASM, Go, Java), optional Bolt | Phase 6 |
| Distributed cluster over TiKV | Phase 7 |

## Quick Start (Embedded)

Atlantis DB is not on crates.io yet. It needs Rust 1.89 or newer (the workspace's
`rust-version`). Depend on the facade crate from a checkout:

```toml
[dependencies]
atlantis-db = { path = "../atlantis-db/crates/atlantis-db" }
tokio = { version = "1", features = ["full"] }
```

```rust
use atlantis_db::{Engine, Value};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::open("./my_atlantis_db").await?;

    engine.execute("CREATE (p:Person {name: 'Alice', age: 34})", &[]).await?;

    let result = engine.execute(
        "MATCH (p:Person) WHERE p.name = $name RETURN p.age AS age",
        &[("name", Value::from("Alice"))],
    ).await?;

    for row in result.rows() {
        println!("age: {}", row.get_int("age").unwrap_or(0));
    }

    engine.shutdown().await?;
    Ok(())
}
```

The [Getting Started](docs/01-getting-started.md) guide lists what works today.

### Records and transactions

The Rust record API works on the same records as Cypher.

```rust
use atlantis_db::{Direction, Engine, Object, Value};

#[tokio::main]
async fn main() -> Result<(), atlantis_db::Error> {
    let engine = Engine::open("./my_atlantis_db").await?;

    // One ACID transaction: commits on Ok, writes nothing on Err.
    engine.transaction(|tx| {
        let mut alice = Object::new();
        alice.insert("id".into(), Value::from("alice"));          // key: Person:alice
        alice.insert("name".into(), Value::from("Alice"));
        let alice = tx.create_node(&["Person"], alice)?;
        let acme = tx.create("Company", None, vec![], Object::new())?; // ULID key
        tx.relate(&alice.id, "WORKS_AT", &acme.id, Object::new())?;
        Ok(())
    }).await?;

    let employers = engine.read(|tx| {
        tx.neighbors(&"Person:alice".parse()?, Direction::Outgoing, Some("WORKS_AT"))
    }).await?;
    println!("{employers:?}");

    // The same data through Cypher.
    let rs = engine.execute("MATCH (:Person {id: 'alice'})-[:WORKS_AT]->(c) RETURN c", &[]).await?;
    assert_eq!(rs.row_count(), 1);

    engine.shutdown().await?; // optional: commits are already durable in records.wal
    Ok(())
}
```

See [API Reference](docs/21-api-reference.md) for the record API and
[Transactions & Consistency](docs/05-transactions-consistency.md) for isolation,
conflicts and durability.

## A Taste of Atlantis Cypher

```cypher
// Implemented — documents, record links, traversal, vector search
CREATE (o:Order {customer: rid('Customer', 'c-17'), address: {city: 'Hue'}});
MATCH (o:Order) WHERE o.address.city = 'Hue' RETURN o.customer.name;
MATCH (a:Person {id: 'alice'})-[:KNOWS*1..3]->(b) RETURN DISTINCT b.name;
CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine'};
CALL db.vector.search('chunk_embedding', $vec, 10) YIELD node, score
RETURN node.text, score;

// Phase 2 — schema
CREATE TABLE Person SCHEMAFULL;
CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';

// Phase 3 — hybrid full-text + vector search, then graph traversal
CALL db.search.hybrid({text: 'doc_text', vector: 'chunk_embedding'},
                      'graph database', $vec, 10) YIELD node, score
MATCH (node)-[:PART_OF]->(d:Doc)
RETURN d.title, score;

// Phase 4 — live query
LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o;
```

```bash
# Phase 5 — standalone server on the default port 7700
atlantis start --bind 0.0.0.0:7700 --user root --pass "$ATLANTIS_PASS" rocksdb://data
```

## Architecture

```
 Clients: Rust embedded | SDKs | CLI | HTTP | WebSocket RPC | Bolt
                         │
 atlantis-server (planned)      protocols, sessions, auth
 atlantis-engine                transactions, permissions, execution, live queries
 atlantis-cypher                parser; planner and cost-based optimizer (planned)
 index layer                    atlantis-index · atlantis-vector · atlantis-text · atlantis-geo
 atlantis-storage               records, edges, adjacency cache (planned)
 atlantis-kv                    transactional ordered KV: mem (today) | rocksdb | tikv
 atlantis-core                  Value, RecordId, errors, config
```

Today the workspace contains `atlantis-core`, `atlantis-kv`, `atlantis-storage`,
`atlantis-vector`, `atlantis-cypher`, `atlantis-engine` and `atlantis-db`. The full crate map, deployment
modes and request flow are in [Architecture](docs/02-architecture.md).

## Performance Targets

These are design targets, not measured results.

| Operation | Target |
|---|---|
| Point lookup (hot data) | < 1ms |
| Graph traversal (hot data) | < 1ms |
| Vector search (top-10 over 1M vectors) | < 10ms |
| Vector ingestion | 100k+ vectors/sec |
| Recall (HNSW, ef=200) | > 95% |

Criterion benchmarks live in `benches/` and run with `cargo bench`.

## Documentation

Start at the [documentation index](docs/README.md). Useful entry points:

- [Getting Started](docs/01-getting-started.md)
- [Architecture](docs/02-architecture.md)
- [Data Model](docs/03-data-model.md)
- [Query Language](docs/06-query-language.md)
- [Implementation Roadmap](docs/25-implementation-roadmap.md)

## Contributing

Contributions are welcome. This project follows standard Rust conventions:

- Run `cargo fmt` before committing
- Run `cargo clippy -- -D warnings` and fix all warnings
- All public API items must have rustdoc comments
- Add tests for new features; benchmarks for performance-sensitive code
- Avoid `unsafe` unless absolutely necessary and documented with a `// SAFETY:` comment

Please open an issue to discuss significant changes before submitting a pull request.

## License

Licensed under the Apache License, Version 2.0, as declared in the workspace
`Cargo.toml`.
