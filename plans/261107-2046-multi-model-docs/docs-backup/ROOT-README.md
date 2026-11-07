<div align="center">

# Atlantis DB

**A next-generation graph database engine for AI-native applications, built in Rust.**

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-2021%2B-orange.svg)](https://www.rust-lang.org)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](#)
[![Crates.io](https://img.shields.io/badge/crates.io-coming%20soon-red.svg)](#)
[![Discord](https://img.shields.io/badge/discord-join%20chat-7289da.svg)](#)

</div>

---

Atlantis DB is a high-performance embedded graph database engine that unifies **graph traversal** and **vector similarity search** into a single query layer. Written entirely in Rust, it is designed from the ground up for AI agent memory, retrieval-augmented generation (RAG), and semantic knowledge graphs.

Inspired by the best ideas from:

- **Neo4j** — the property graph model and Cypher query language
- **FalkorDB** — in-memory graph performance and Redis-like simplicity
- **ClickHouse** — analytical throughput and columnar efficiency

Atlantis DB brings them together in one engine optimized for the workloads that matter most to AI applications: storing relationships and embeddings together, and querying across both in a single operation.

## Why Atlantis DB?

Traditional databases force you to choose: a graph database for relationships, or a vector database for similarity. AI applications need both, and stitching them together with sync jobs and glue code is fragile and slow.

Atlantis DB stores your graph and your vectors in one engine. A single Cypher query can find semantically similar content and then traverse its relationships, all without leaving the database or changing query languages.

```
┌─────────────────────────────────────────────────────┐
│                   Atlantis DB Engine                 │
│                                                       │
│  ┌──────────┐   ┌──────────┐   ┌──────────────────┐ │
│  │  Cypher   │   │  HNSW    │   │  Tiered Storage  │ │
│  │  Query    │──▶│  Vector  │──▶│  Hot: in memory  │ │
│  │  Engine   │   │  Index   │   │  Cold: on disk   │ │
│  └──────────┘   └──────────┘   └──────────────────┘ │
│        │              │              │                │
│        ▼              ▼              ▼                │
│  ┌─────────────────────────────────────────────────┐ │
│  │           Property Graph Data Model             │ │
│  │  Nodes · Relationships · Labels · Properties    │ │
│  │  + Dedicated Embedding Nodes (vectors)          │ │
│  └─────────────────────────────────────────────────┘ │
│                                                       │
│  Concurrency: Actor-based (tokio) · Lock-free         │
└─────────────────────────────────────────────────────┘
```

## Key Features

- **Cypher-compatible** — Write the same Cypher queries you know from Neo4j and FalkorDB. No new language to learn.
- **Hybrid vector + graph** — Embeddings live on dedicated nodes inside the graph. Vector search results are graph nodes you can traverse immediately.
- **HNSW vector index** — High-recall approximate nearest neighbor search with sub-10ms latency over millions of vectors.
- **Hybrid tiered storage** — Hot data stays in memory for speed; cold data pages to disk automatically. Datasets can exceed available RAM.
- **Actor-based concurrency** — A lock-free tokio actor model keeps the core engine simple, safe, and predictable under concurrent load.
- **Embedded library** — No server to deploy. Add Atlantis DB to your Rust application as a dependency and open a database in-process.
- **Rust-native** — Zero-cost abstractions, memory safety without garbage collection, and fearless concurrency.

## Quick Start

Add Atlantis DB to your `Cargo.toml`:

```toml
[dependencies]
atlantis-db = "0.1"
tokio = { version = "1", features = ["full"] }
```

Open a database and run a query:

```rust
use atlantis_db::{Engine, Config};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open or create a database at the given path
    let engine = Engine::open("./my_atlantis_db").await?;

    // Create nodes and relationships
    engine.execute(
        "CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})",
        &[]
    ).await?;

    // Query the graph
    let result = engine.execute(
        "MATCH (a:Person)-[:KNOWS]->(b:Person)
         WHERE a.name = $name
         RETURN b.name AS friend",
        &[("name", "Alice".into())]
    ).await?;

    for row in result.rows() {
        println!("Friend: {}", row.get_string("friend")?);
    }

    Ok(())
}
```

## AI / RAG Usage

Atlantis DB maps naturally to retrieval-augmented generation patterns. Embeddings are stored as dedicated nodes linked to content:

```rust
// Insert content with an embedding
engine.execute(
    "CREATE (d:Document {title: 'Rust Ownership', body: '...'})
     CREATE (e:Embedding {vector: $vec})
     CREATE (e)-[:EMBEDS]->(d)",
    &[("vec", vec![0.12, 0.45, 0.78, /* ... */].into())]
).await?;

// Find similar documents, then traverse to related content
let results = engine.execute(
    "CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
     YIELD node, score
     MATCH (node)-[:EMBEDS]->(doc:Document)
     RETURN doc.title AS title, doc.body AS body, score
     ORDER BY score DESC",
    &[("query_vec", query_embedding.into())]
).await?;
```

A single query performs vector similarity search and graph traversal together — no separate vector store, no sync step.

## Architecture

Atlantis DB is organized as a Cargo workspace with focused crates:

```
atlantis-db/
├── crates/
│   ├── atlantis-core/      # Core types: Node, Relationship, Property, Value
│   ├── atlantis-storage/   # Hybrid tiered storage (in-memory + on-disk)
│   ├── atlantis-vector/    # HNSW vector index
│   ├── atlantis-cypher/    # Cypher lexer, parser, and query planner
│   ├── atlantis-engine/    # Actor-based engine and query executor
│   └── atlantis-db/        # Public API facade (the crate users depend on)
├── benches/                # Criterion benchmarks
├── tests/                  # Integration tests
└── examples/               # Runnable usage examples
```

| Crate | Responsibility |
|---|---|
| `atlantis-core` | Shared types, traits, and the property graph data model |
| `atlantis-storage` | Hybrid tiered storage with automatic eviction (hot in-memory, cold on-disk) |
| `atlantis-vector` | HNSW index construction, search, and persistence |
| `atlantis-cypher` | Cypher lexer, recursive-descent parser, logical planner, and optimizer |
| `atlantis-engine` | Tokio actor-based runtime, command processing, background tasks (tiering, checkpointing) |
| `atlantis-db` | User-facing API: `Engine`, `Config`, `ResultSet`, parameterized queries |

## How It Compares

| Feature | Atlantis DB | Neo4j | FalkorDB | Qdrant | pgvector |
|---|---|---|---|---|---|
| Language | Rust | Java | C | Rust | C (Postgres) |
| Query language | Cypher | Cypher | Cypher | Custom API | SQL |
| Graph model | Yes | Yes | Yes | No | No |
| Vector search | Yes (HNSW) | Via plugin | Yes (HNSW) | Yes (HNSW) | Yes (HNSW) |
| Hybrid queries (graph + vector) | Yes | Via plugin | Limited | No | Via SQL joins |
| Embedded library | Yes | No | No | No | No (extension) |
| Tiered storage | Yes | Yes | No | Yes | Yes (Postgres) |
| AI agent memory optimized | Yes | No | No | Partial | No |

## Performance Targets

| Operation | Target |
|---|---|
| Point lookup (hot data) | < 1ms |
| Graph traversal (hot data) | < 1ms |
| Vector search (top-10 over 1M vectors) | < 10ms |
| Vector ingestion | 100k+ vectors/sec |
| Recall (HNSW, ef=200) | > 95% |

Benchmarks are runnable via `cargo bench` and tracked for regressions.

## Roadmap

- [x] Requirements & architecture design
- [ ] Core types and property graph model
- [ ] Hybrid tiered storage engine
- [ ] HNSW vector index
- [ ] Cypher lexer and parser
- [ ] Query planner and executor
- [ ] Actor-based engine runtime
- [ ] Public API and documentation
- [ ] Benchmarks and performance tuning
- [ ] Standalone server mode (v2)
- [ ] Client SDKs (v2)
- [ ] Distributed clustering (v3)

See the [spec docs](.kiro/spec/atlantis-db/requirements.md) for the full requirements.

## Contributing

Contributions are welcome. This project follows standard Rust conventions:

- Run `cargo fmt` before committing
- Run `cargo clippy -- -D warnings` and fix all warnings
- All public API items must have rustdoc comments
- Add tests for new features; benchmarks for performance-sensitive code
- Avoid `unsafe` unless absolutely necessary and documented with a `// SAFETY:` comment

Please open an issue to discuss significant changes before submitting a pull request.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

<div align="center">

**[Documentation](#)** · **[Examples](#)** · **[Benchmarks](#)** · **[Contributing](#)**

Built with Rust · Powered by graphs and vectors

</div>
