# Atlantis DB — Requirements

## Overview

Atlantis DB is a next-generation embedded graph database engine written in Rust, optimized for AI agent memory and retrieval. It combines a Cypher-compatible query language with hybrid vector-graph storage, enabling queries that seamlessly blend graph traversals with semantic similarity search. Inspired by Neo4j (graph model + Cypher), FalkorDB (in-memory performance), and ClickHouse (analytical throughput), Atlantis DB is designed as a complete end-to-end data solution delivered as an embeddable Rust library.

## Goals

- Provide an embeddable Rust library (`atlantis-db`) with a clean in-process API for storing and querying property graphs with integrated vector embeddings.
- Implement a Cypher-compatible query language so developers familiar with Neo4j/FalkorDB can adopt Atlantis DB with minimal friction.
- Support hybrid queries that combine graph traversal with vector similarity search in a single query plan.
- Deliver a hybrid tiered storage engine (hot data in memory, cold data on disk with automatic tiering) so datasets can exceed RAM without sacrificing hot-path performance.
- Use HNSW as the vector index for efficient approximate nearest neighbor search with high recall.
- Use an actor-based concurrency model (tokio) for naturally safe, lock-free concurrent access.
- Map well to RAG and AI agent memory patterns through dedicated embedding nodes connected to content nodes.
- Ship with a full testing pyramid: unit tests, integration tests, and criterion benchmarks.

## Non-Goals (for v1)

- A standalone server with a wire protocol or HTTP API. v1 is an embedded library only.
- Client SDKs in languages other than Rust.
- Gremlin or custom DSL query languages. v1 focuses on Cypher compatibility.
- AI framework integrations (LangChain, etc.) or data connectors.
- A CLI tool or observability/metrics dashboard.
- Multi-node clustering or distributed deployment.

## Functional Requirements

### FR1: Property Graph Data Model

The database must support the property graph model:

- **Nodes**: Entities with zero or more labels and a map of properties (key-value pairs). Property values support: strings, integers, floats, booleans, null, and arrays of primitives.
- **Relationships**: Directed edges between two nodes with exactly one relationship type and a map of properties.
- **Labels**: Nodes can have multiple labels (e.g., `:Person:Actor`).
- **Relationship types**: Each relationship has exactly one type (e.g., `:ACTED_IN`).
- **Schema flexibility**: The graph is schemaless by default; labels and relationship types are created implicitly.

### FR2: Dedicated Embedding Nodes (Vector + Graph Hybrid)

Vectors are first-class graph citizens via dedicated embedding nodes:

- **Embedding nodes**: Specialized nodes with a label (e.g., `:Embedding`) that carry a vector property (fixed-dimension float array) plus optional metadata.
- **Content nodes**: Regular nodes representing the source content (documents, chunks, concepts, etc.).
- **Linkage**: Embedding nodes connect to content nodes via relationships (e.g., `:EMBEDS`), enabling graph traversal from a similarity match back to the source content.
- **Multiple embeddings per content**: A content node may have multiple embedding nodes (different models, different chunks) linked via separate relationships.
- **Vector dimensions**: Configurable per HNSW index; all vectors within one index share the same dimensionality.

### FR3: Cypher-Compatible Query Language

The database must implement a Cypher-compatible query language:

- **MATCH clauses**: Pattern matching with node and relationship patterns, variable-length paths, and optional matches.
- **WHERE clauses**: Filtering on node/relationship properties, including comparison, string, and logical operators.
- **RETURN clauses**: Projection of nodes, relationships, properties, and expressions.
- **CREATE / MERGE / DELETE clauses**: Graph mutations (create nodes/relationships, merge upserts, delete).
- **SET / REMOVE clauses**: Property updates.
- **WITH clauses**: Query chaining and sub-query piping.
- **ORDER BY / LIMIT / SKIP**: Result ordering and pagination.
- **Vector similarity operators**: Cypher-extended functions for similarity search:
  - `db.vector.search(label, query_vector, k, score_threshold)` — returns top-k similar embedding nodes.
  - `db.vector.similarity(a, b)` — cosine similarity between two vectors.
  - Vector predicates in WHERE clauses (e.g., `WHERE db.vector.similarity(n.embedding, $query) > 0.8`).
- **Compatibility target**: A practical subset of Cypher covering common query patterns from Neo4j/FalkorDB. Full Cypher spec compliance is not required for v1, but the supported syntax must be syntactically valid Cypher.

### FR4: Hybrid Query Execution

Queries that combine graph traversal and vector search must be supported in a single query plan:

- A query can start from a vector similarity search and traverse outward via relationships (RAG pattern: find similar embeddings → traverse to content → traverse to related content).
- A query can start from a graph pattern match and filter by vector similarity (find nodes matching a pattern, then rank by similarity to a query vector).
- The query planner must recognize vector search operations and route them to the HNSW index rather than scanning all nodes.

### FR5: Hybrid Tiered Storage

The storage engine must implement tiered storage:

- **Hot tier**: Frequently accessed nodes, relationships, and index structures reside in memory for low-latency access.
- **Cold tier**: Infrequently accessed data is persisted to disk (using a page-based or LSM-based approach) and loaded on demand.
- **Automatic tiering**: A background eviction policy (configurable, e.g., LRU, LFU, or clock-based) moves cold data to disk and promotes hot data to memory.
- **Durability**: Disk-persisted data must survive process restarts. On startup, the in-memory hot tier is reconstructed from disk.
- **Configurability**: Memory budget, eviction policy, and disk path are configurable via the engine config.

### FR6: HNSW Vector Index

The vector index must implement HNSW:

- **Index construction**: Incremental insertion of vectors with configurable parameters (`M`, `ef_construction`, `max_level`).
- **Search**: Approximate nearest neighbor search returning top-k results with configurable `ef_search`.
- **Distance metric**: Cosine similarity (normalized dot product) as the default; pluggable interface for Euclidean and dot product.
- **Deletion**: Support for vector deletion (marked deletion with periodic compaction or immediate removal).
- **Persistence**: HNSW graph structure is persisted to disk as part of the cold tier and loaded on startup.
- **Concurrency**: As a single-writer actor, the HNSW index receives insert/search commands via messages; no internal locking needed.

### FR7: Actor-Based Concurrency Model

The engine uses a tokio actor-based architecture:

- **Core engine actor**: Owns the storage engine, vector index, and query executor. Processes one operation at a time (serialized), guaranteeing no data races.
- **Client interface**: Callers send async messages (commands) to the engine actor and await responses. The public API wraps this in ergonomic async methods.
- **Background actors**: Separate actors for tiering/eviction, persistence/checkpointing, and index compaction, communicating with the core actor via channels.
- **Graceful shutdown**: The engine must support clean shutdown, flushing in-memory state to disk.

### FR8: Embedded Library API

The public API must be usable as an embedded Rust library:

- **Engine initialization**: `Engine::open(config)` or `Engine::open(path)` to open or create a database.
- **Query execution**: `engine.execute(cypher, params)` returns a `ResultSet` (rows of values).
- **Parameters**: Cypher queries support parameterized queries (`$param`) to prevent injection and improve performance.
- **Transactions**: Optional explicit transaction API for grouping multiple operations. Auto-commit by default.
- **Error handling**: All fallible operations return `Result<T, AtlantisError>` with a typed error enum.
- **Thread safety**: The engine handle is cheaply clonable (`Arc`-based) and safe to share across async tasks.

## Non-Functional Requirements

### NFR1: Performance

- **Hot-path query latency**: Sub-millisecond for simple point lookups and traversals on hot (in-memory) data.
- **Vector search latency**: Sub-10ms for top-k search over 1M vectors with high recall.
- **Ingest throughput**: Target 100k+ vector insertions per second on commodity hardware.
- **Benchmarks**: Criterion benchmarks must cover core operations (insert, traverse, vector search, hybrid query) and be runnable via `cargo bench`.

### NFR2: Memory Efficiency

- The in-memory hot tier must have a configurable memory budget and evict to disk when exceeded.
- Vector storage must use compact representations (e.g., `Vec<f32>` with pre-allocation, not boxed per element).
- Graph storage should use compact adjacency representations (CSR or similar) for in-memory hot data.

### NFR3: Correctness & Testing

- **Unit tests**: Per-crate unit tests covering core logic (parser, planner, storage, vector index).
- **Integration tests**: Tests exercising the full embedded API end-to-end (open engine, create data, query, verify results).
- **Property-based tests**: Use `proptest` for invariant checking on the parser and storage engine.
- **Benchmarks**: Criterion benchmarks for performance-sensitive operations with regression detection.
- **Test data**: Provide a small example graph dataset for testing (e.g., a movie graph like Neo4j's classic dataset).

### NFR4: Rust Best Practices & Code Quality

- **Edition**: Rust 2021 edition (or latest stable at setup time).
- **Formatting**: `rustfmt` with project-specific configuration.
- **Linting**: `clippy` with `-D warnings` enforced; no clippy warnings allowed.
- **Error handling**: Use `thiserror` for library error types, `anyhow` for application/binaries only.
- **Dependency**: Prefer well-maintained, popular crates. Pin versions in `Cargo.toml`.
- **Documentation**: All public API items must have rustdoc comments. `cargo doc` must build without warnings.
- **Unsafe code**: Avoid `unsafe` unless absolutely necessary and documented with a `// SAFETY:` comment.

## Tech Stack

| Concern | Choice | Rationale |
|---|---|---|
| Language | Rust (2021 edition) | Performance, safety, zero-cost abstractions |
| Async runtime | tokio | Industry-standard async actor model |
| Property graph model | Labels + properties on nodes/relationships | Neo4j-compatible model |
| Vector index | HNSW | Best recall/performance balance for ANN |
| Storage | Hybrid tiered (in-memory + on-disk) | Scale beyond RAM while keeping hot data fast |
| Query language | Cypher (compatible subset) | Ecosystem familiarity |
| Error types | thiserror | Ergonomic typed errors for libraries |
| Testing | built-in + proptest + criterion | Full testing pyramid |
| Serialization | serde + bincode | Compact binary persistence |
| Logging | tracing | Structured async-aware logging |

## Open Questions

These are deferred to the design phase and do not block requirements sign-off:

1. **Disk format**: Should the on-disk cold tier use a page-based file format (mmap), an LSM-tree, or a simple append-only log? This is an architectural design decision.
2. **Cypher parser implementation**: Hand-written recursive descent parser or use a parser combinator library (`chumsky`, `nom`) or `pest` PEG grammar? Design decision.
3. **HNSW parameters defaults**: What are sensible default values for `M`, `ef_construction`, `ef_search`? To be determined during design and tuned via benchmarks.
4. **Graph adjacency representation**: CSR (Compressed Sparse Row), adjacency hash maps, or linked lists for in-memory storage? Design decision with performance implications.
5. **Tiering eviction algorithm**: LRU vs LFU vs clock vs adaptive? To be evaluated during design.
