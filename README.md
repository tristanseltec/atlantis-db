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
> with two stores that do not share data yet: a Cypher graph + vector engine, and a
> transactional record store (documents, graph nodes and edges in namespaced tables) on
> an in-memory MVCC key-value layer. Everything else in this README is target design,
> marked with the phase that delivers it. See the
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
| Embedded Rust library (`atlantis-db`) | Implemented (in-memory or file path; one engine per directory) |
| Property graph model, Cypher subset (single-node `MATCH`, `WHERE`, `RETURN`, `ORDER BY`, `SKIP`, `LIMIT`) | Partial |
| Relationship creation, multi-hop and variable-length traversal, `MATCH ... SET`, `MERGE` | Phase 0 |
| HNSW vector index | Partial (index code exists; the engine does not populate it until Phase 0) |
| Hot/cold tiered storage with CSR + delta adjacency | Partial (eviction and auto-checkpoint not wired until Phase 0) |
| Transactional KV layer (`atlantis-kv`), MVCC, snapshot and serializable isolation | Implemented (in-memory `mem` backend, durable only at checkpoints) |
| Record transactions in Rust (`engine.transaction`, `read`, retry on conflict) | Implemented (record store only; not visible to Cypher yet) |
| RocksDB backend, sync modes, Cypher on the KV layer, `BEGIN`/`COMMIT` | Phase 1 |
| Record model: namespaces, databases, tables, `RecordId`, documents, edge tables, label index, extended value types | Partial (Rust record API; `Decimal`, `Range` and Cypher access Phase 2) |
| Schema DDL, record links, joins, secondary indexes | Phase 2 |
| Full-text search, geospatial, key-value API, time-series tables, vector index DDL, graph algorithms | Phase 3 |
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

The [Getting Started](docs/01-getting-started.md) guide lists exactly which Cypher
queries work today.

### Records and transactions

Records live in a separate record store for now, so Cypher queries do not see them.

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

    engine.shutdown().await?; // writes records.kv; commits after the last checkpoint are lost on a crash
    Ok(())
}
```

See [API Reference](docs/21-api-reference.md) for the record API and
[Transactions & Consistency](docs/05-transactions-consistency.md) for isolation,
conflicts and durability.

## A Taste of Atlantis Cypher (Planned)

```cypher
// Phase 2 — schema, documents and record links
CREATE TABLE Person SCHEMAFULL;
CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';
CREATE (o:Order {customer: rid('Customer', 'c-17'), address: {city: 'Hue'}});
MATCH (o:Order) WHERE o.address.city = 'Hue' RETURN o.customer.name;

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
 atlantis-cypher                parser, planner, cost-based optimizer
 index layer                    atlantis-index · atlantis-vector · atlantis-text · atlantis-geo
 atlantis-storage               records, edges, CSR adjacency cache
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
