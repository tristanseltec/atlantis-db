# 01 — Getting Started

This guide walks through installing Atlantis DB, opening a database, creating graph data, and running queries including vector similarity search.

## Prerequisites

- **Rust 1.75+** (2021 edition)
- A tokio-compatible async runtime (included automatically)

## Adding the Dependency

Add Atlantis DB to your `Cargo.toml`:

```toml
[dependencies]
atlantis-db = "0.1"
tokio = { version = "1", features = ["full"] }
```

## Opening a Database

Atlantis DB is an embedded library — no server process to run. Open a database in-process:

```rust
use atlantis_db::Engine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Opens or creates a database at the given path.
    // If a checkpoint exists, the database state is restored from disk.
    let engine = Engine::open("./my_atlantis_db").await?;

    // ... run queries ...

    // Shut down (flushes in-memory state to disk if a disk path is configured)
    engine.shutdown().await?;
    Ok(())
}
```

For testing or ephemeral use, open an in-memory database with no disk persistence:

```rust
let engine = Engine::open_in_memory().await?;
```

For custom configuration (memory budget, vector dimensions, HNSW parameters), see [Configuration Reference](12-configuration.md):

```rust
use atlantis_db::{Engine, Config, DistanceMetricKind};

let config = Config::builder()
    .disk_path("./my_db")
    .vector_dimensions(384)
    .distance_metric(DistanceMetricKind::Cosine)
    .build();

let engine = Engine::open_with_config(config).await?;
```

## Creating Nodes and Relationships

Use Cypher `CREATE` to build your graph:

```rust
engine.execute(
    "CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})",
    &[],
).await?;
```

This creates two `:Person` nodes with a `:KNOWS` relationship between them. Labels and relationship types are created implicitly — the graph is schemaless by default.

## Querying with MATCH

```rust
let result = engine.execute(
    "MATCH (a:Person)-[:KNOWS]->(b:Person)
     WHERE a.name = $name
     RETURN b.name AS friend",
    &[("name", "Alice".into())],
).await?;

for row in result.rows() {
    println!("Friend: {}", row.get_string("friend").unwrap_or(""));
}
```

Key points:
- `$name` is a parameter placeholder, supplied as a `&[(&str, Value)]` slice.
- `RETURN b.name AS friend` projects a property with an alias.
- The `ResultSet` provides typed accessors: `get_string`, `get_int`, `get_float`, `get_bool`, `get_vector`.

## Vector Similarity Search

Atlantis DB stores embeddings as dedicated `:Embedding` nodes linked to content via relationships. Here is the RAG pattern:

```rust
// Insert content with an embedding
engine.execute(
    "CREATE (d:Document {title: 'Rust Ownership', body: '...'})
     CREATE (e:Embedding {vector: $vec})
     CREATE (e)-[:EMBEDS]->(d)",
    &[("vec", vec![0.12, 0.45, 0.78, /* ... */].into())],
).await?;
```

Search for similar embeddings, then traverse to the content they embed:

```rust
let results = engine.execute(
    "CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
     YIELD node, score
     MATCH (node)-[:EMBEDS]->(doc:Document)
     RETURN doc.title AS title, doc.body AS body, score
     ORDER BY score DESC",
    &[("query_vec", query_embedding.into())],
).await?;

for row in results.rows() {
    println!("Title: {} (score: {:.3})",
        row.get_string("title").unwrap_or(""),
        row.get_float("score").unwrap_or(0.0));
}
```

A single query performs vector similarity search and graph traversal together — no separate vector store, no sync step.

### How `db.vector.search` Works

The `CALL db.vector.search(label, query_vector, k, score_threshold)` procedure:

| Argument | Type | Description |
|---|---|---|
| `label` | String | The node label to search (e.g., `'Embedding'`) |
| `query_vector` | Vector (`Vec<f32>`) | The query embedding |
| `k` | Integer | Number of nearest neighbors to return |
| `score_threshold` | Float (optional) | Minimum similarity score (0.0–1.0); results below this are filtered out |

It `YIELD`s two variables: `node` (the matching `NodeId`) and `score` (cosine similarity, 0.0–1.0 where 1.0 is identical).

## Where to Go Next

- **[Architecture Overview](02-architecture.md)** — Understand the system design and data flow.
- **[Query Cookbook](10-query-cookbook.md)** — More query patterns and examples.
- **[RAG Patterns](11-rag-patterns.md)** — Deep dive into building RAG pipelines.
- **[Configuration Reference](12-configuration.md)** — Tune storage, vector index, and HNSW parameters.
- **[API Reference](09-api-reference.md)** — Complete API documentation.
