# 11 — RAG Patterns

Retrieval-augmented generation (RAG) and AI agent memory are the primary use cases Atlantis DB is designed for. This chapter covers practical patterns for building RAG pipelines using the integrated vector + graph model.

## The Core Idea

Traditional RAG requires two systems: a vector database for similarity search and a graph/document store for content. Atlantis DB unifies them. Embeddings live on dedicated `:Embedding` nodes connected to content via relationships. A single Cypher query performs vector search and graph traversal together.

```
(:Document {title, body})
        ▲
        │ :EMBEDS
        │
(:Embedding {vector: [0.12, 0.45, ...]})
```

## Pattern 1: Basic Document RAG

Store documents with their embeddings, then search by similarity and retrieve the content.

### Ingestion

```rust
// Create a document node
engine.execute(
    "CREATE (d:Document {title: $title, body: $body, url: $url})",
    &[
        ("title", "Rust Ownership".into()),
        ("body", "Ownership is Rust's most unique feature...".into()),
        ("url", "https://doc.rust-lang.org/ownership".into()),
    ],
).await?;

// Create an embedding node linked to the document
engine.execute(
    "CREATE (e:Embedding {vector: $vec})-[:EMBEDS]->(d:Document {title: $title})",
    &[
        ("vec", embedding_vector.into()),  // Vec<f32> from your embedding model
        ("title", "Rust Ownership".into()),
    ],
).await?;
```

**Note**: In the current implementation, relationship creation in `CREATE` is not yet wired through to the executor. As a workaround, create nodes and relationships separately using the direct API:

```rust
// Workaround: create nodes and edges separately
let result = engine.execute(
    "CREATE (d:Document {title: $title, body: $body}) RETURN d",
    &[("title", "Rust Ownership".into()), ("body", "...".into())],
).await?;

// Then create the embedding and link via the store
// (Future: full CREATE relationship support in Cypher)
```

See [Implementation Roadmap](13-implementation-roadmap.md) for when CREATE relationships will be fully supported.

### Retrieval

```rust
let results = engine.execute(
    "CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
     YIELD node, score
     MATCH (node)-[:EMBEDS]->(doc:Document)
     RETURN doc.title AS title, doc.body AS body, doc.url AS url, score
     ORDER BY score DESC",
    &[("query_vec", query_embedding.into())],
).await?;

for row in results.rows() {
    println!(
        "Title: {} (score: {:.3})\nURL: {}\n",
        row.get_string("title").unwrap_or(""),
        row.get_float("score").unwrap_or(0.0),
        row.get_string("url").unwrap_or(""),
    );
}
```

The query flow:
1. `db.vector.search` finds the 10 most similar `:Embedding` nodes.
2. `MATCH (node)-[:EMBEDS]->(doc:Document)` traverses from each embedding to its document.
3. `RETURN` projects the document content and similarity score.
4. `ORDER BY score DESC` ranks by relevance.

## Pattern 2: Chunked Documents

For long documents, split into chunks with separate embeddings per chunk. Each chunk links to the parent document.

```
(:Document {title: "Long Article"})
    ▲
    ├── :HAS_CHUNK → (:Chunk {index: 0, text: "First paragraph..."})
    │                    ▲
    │                    │ :EMBEDS
    │                    │
    │              (:Embedding {vector: [...]})
    │
    ├── :HAS_CHUNK → (:Chunk {index: 1, text: "Second paragraph..."})
    │                    ▲
    │                    │ :EMBEDS
    │                    │
    │              (:Embedding {vector: [...]})
    │
    └── :HAS_CHUNK → (:Chunk {index: 2, text: "Third paragraph..."})
                         ▲
                         │ :EMBEDS
                         │
                   (:Embedding {vector: [...]})
```

### Retrieval with Chunk Context

```cypher
CALL db.vector.search('Embedding', $query_vec, 5, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS]->(chunk:Chunk)-[:HAS_CHUNK]-(doc:Document)
RETURN doc.title AS title, chunk.text AS text, chunk.index AS chunk_index, score
ORDER BY score DESC
```

This finds similar chunks, traverses to the parent document, and returns both the chunk text and the document title.

## Pattern 3: Multi-Model Embeddings

A single content node can have multiple embeddings from different models, linked via different relationship types.

```
(:Document {title: "Article"})
    ├── :EMBEDS_OPENAI   → (:Embedding {vector: [...], model: "text-embedding-3-small"})
    ├── :EMBEDS_MINI      → (:Embedding {vector: [...], model: "all-MiniLM-L6-v2"})
    └── :EMBEDS_COHERE    → (:Embedding {vector: [...], model: "embed-english-v3"})
```

Search with a specific model:

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS_OPENAI]->(doc:Document)
WHERE node.model = 'text-embedding-3-small'
RETURN doc.title AS title, score
ORDER BY score DESC
```

**Note**: The `db.vector.search` procedure searches all vectors in the index regardless of model. Label-based filtering on the embedding node is not yet applied in the executor. For multi-model setups, consider separate indexes or filter results post-query. See [Implementation Roadmap](13-implementation-roadmap.md).

## Pattern 4: Knowledge Graph RAG

Combine semantic search with graph relationships for richer retrieval. For example, a knowledge graph where documents cite each other:

```
(:Document {title: "Paper A"})-[:CITES]->(:Document {title: "Paper B"})
         ▲                                              ▲
         │ :EMBEDS                                       │ :EMBEDS
         │                                               │
   (:Embedding {vector})                          (:Embedding {vector})
```

### Find Similar, Then Expand to Citations

```cypher
CALL db.vector.search('Embedding', $query_vec, 5, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS]->(doc:Document)
OPTIONAL MATCH (doc)-[:CITES]->(cited:Document)
RETURN doc.title AS title, score,
       collect(cited.title) AS citations
ORDER BY score DESC
```

This finds similar documents and retrieves their citation chains in a single query. **Note**: `collect()` aggregation and `OPTIONAL MATCH` are not yet implemented. See the roadmap.

## Pattern 5: AI Agent Memory

Store conversation context and agent memory as a graph with embeddings:

```
(:Agent {id: "agent-1"})
    ├── :REMEMBERS → (:Memory {text: "User likes Rust", timestamp: ...})
    │                      ▲
    │                      │ :EMBEDS
    │                      │
    │                (:Embedding {vector: [...]})
    │
    └── :REMEMBERS → (:Memory {text: "User asked about ownership", timestamp: ...})
                           ▲
                           │ :EMBEDS
                           │
                     (:Embedding {vector: [...]})
```

### Recall Relevant Memories

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS]->(memory:Memory)<-[:REMEMBERS]-(agent:Agent {id: $agent_id})
RETURN memory.text AS memory, memory.timestamp AS timestamp, score
ORDER BY score DESC LIMIT 5
```

Retrieve the most relevant memories for an agent, ranked by semantic similarity. The graph traversal ensures you only get memories belonging to the specific agent.

## Best Practices

### Embedding Node Design

- Use a dedicated `:Embedding` label for embedding nodes.
- Store the vector in a `vector` property.
- Link to content via a descriptive relationship type (e.g., `:EMBEDS`, `:EMBEDS_OPENAI`).
- Store metadata (model name, chunk index, creation timestamp) on the embedding node.

### Dimension Configuration

Set the vector dimensions in the config to match your embedding model:

```rust
let config = Config::builder()
    .vector_dimensions(1536)  // OpenAI text-embedding-3-small
    .distance_metric(DistanceMetricKind::Cosine)
    .build();
```

All vectors in a single HNSW index must share the same dimensionality. The index validates this on insertion and returns `DimensionMismatch` errors for mismatched vectors.

### Distance Metric Selection

- **Cosine** (default): Best for most embedding models. Handles varying vector magnitudes.
- **Euclidean**: For spatial/geometric data or models that produce L2-normalized vectors.
- **DotProduct**: For models that require maximum inner product search (pre-normalized vectors).

### Score Threshold Tuning

The `score_threshold` parameter in `db.vector.search` filters results by minimum cosine similarity (0.0–1.0, where 1.0 is identical). Start with 0.7 and adjust based on your recall/precision trade-off:
- Higher threshold (0.8–0.9): Fewer, more relevant results.
- Lower threshold (0.5–0.6): More results, possibly less relevant.
- No threshold: Return all top-k results regardless of score.

### HNSW Parameter Tuning for RAG

For RAG workloads where recall matters more than raw latency:

```rust
let config = Config::builder()
    .vector_dimensions(384)
    .hnsw_params(HnswParams {
        m: 24,              // more connections for better recall
        m0: 48,
        ef_construction: 300,  // higher quality index build
        ef_search: 100,     // larger search pool for better recall
        max_level: 16,
    })
    .build();
```

## What's Next

- **[10 — Query Cookbook](10-query-cookbook.md)** — More query patterns.
- **[05 — Vector Index](05-vector-index.md)** — HNSW algorithm and parameter details.
- **[12 — Configuration Reference](12-configuration.md)** — Full configuration options.
