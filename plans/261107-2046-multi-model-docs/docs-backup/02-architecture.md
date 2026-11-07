# 02 — Architecture Overview

Atlantis DB is organized as a Cargo workspace of six focused crates. This chapter covers the system-level architecture, the dependency graph between crates, the data flow through the query pipeline, and the core design principles.

## System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      User Application                         │
│                                                               │
│                  atlantis-db (Public API)                     │
│                  ┌──────────────────────────┐                 │
│                  │  Engine                  │                 │
│                  │  Config · ResultSet · Row│                 │
│                  └────────────┬─────────────┘                 │
│                               │ commands                      │
│                  ┌────────────▼─────────────┐                 │
│                  │    atlantis-engine        │                 │
│                  │  ┌─────────────────────┐  │                 │
│                  │  │ CoreEngineActor     │  │                 │
│                  │  │  (tokio, lock-free)  │  │                 │
│                  │  └──┬───────┬───────┬──┘  │                 │
│                  │     │       │       │      │                 │
│                  │  ┌──▼──┐ ┌─▼────┐ ┌▼───┐  │                 │
│                  │  │Exec-│ │Vector│ │BG  │  │                 │
│                  │  │utor │ │Index │ │Actors│ │                 │
│                  │  └──┬──┘ └──┬───┘ └────┘  │                 │
│                  └─────┼───────┼──────────────┘                 │
│                        │       │                                │
│           ┌────────────▼──┐ ┌──▼───────────┐                   │
│           │atlantis-cypher│ │atlantis-vector│                   │
│           │ Lexer→Parser  │ │   HNSW Index   │                   │
│           │ →Planner      │ │ Distance Metrics│                   │
│           └───────────────┘ └──────────────┘                   │
│                        │       │                                │
│           ┌────────────▼───────▼────────┐                       │
│           │      atlantis-storage       │                       │
│           │  ┌────────┐  ┌───────────┐  │                       │
│           │  │Hot Tier│  │ Cold Tier │  │                       │
│           │  │(memory)│  │  (disk)   │  │                       │
│           │  │CSR+Delta│  │Append-only│ │                       │
│           │  └────────┘  └───────────┘  │                       │
│           └────────────────────────────┘                       │
│                        │                                        │
│           ┌────────────▼────────────────┐                      │
│           │       atlantis-core          │                      │
│           │  Value · Node · Relationship │                      │
│           │  Label · RelType · Property  │                      │
│           │  Config · Error              │                      │
│           └──────────────────────────────┘                      │
└─────────────────────────────────────────────────────────────┘
```

## Crate Dependency Graph

The crates form a layered dependency chain, with `atlantis-core` at the foundation:

```
atlantis-core          ← no internal dependencies
    ↑
atlantis-storage       ← depends on atlantis-core
atlantis-vector        ← depends on atlantis-core
atlantis-cypher        ← depends on atlantis-core
    ↑
atlantis-engine        ← depends on atlantis-core, atlantis-storage,
                         atlantis-vector, atlantis-cypher
    ↑
atlantis-db            ← depends on atlantis-engine (re-exports public API)
```

| Crate | Depends On | Responsibility |
|---|---|---|
| `atlantis-core` | serde, thiserror | Shared types, traits, config, errors |
| `atlantis-storage` | atlantis-core | Hot/cold tiered graph storage with CSR + delta |
| `atlantis-vector` | atlantis-core, rand | HNSW index, distance metrics, vector storage |
| `atlantis-cypher` | atlantis-core | Lexer, parser, AST, logical planner, optimizer |
| `atlantis-engine` | core, storage, vector, cypher, tokio | Actor runtime, query executor, command protocol |
| `atlantis-db` | atlantis-engine (and re-exports from core) | Public API facade: `Engine`, `Config`, `ResultSet` |

The dependency direction is strictly downward — lower crates never depend on higher ones. This keeps the layering clean and testable.

## Query Data Flow

When a user calls `engine.execute(cypher, params)`, the query travels through the full pipeline:

```
engine.execute("MATCH (n:Person) WHERE n.name = $name RETURN n.name", params)
  │
  ▼
┌─────────────────────────────────────────────────────────┐
│  1. atlantis-cypher::lexer::tokenize(cypher)             │
│     "MATCH (n:Person)..." → Vec<SpannedToken>            │
│                                                          │
│  2. atlantis-cypher::parser::parse(tokens)               │
│     Vec<SpannedToken> → Statement (AST)                 │
│                                                          │
│  3. atlantis-cypher::planner::plan(statement)            │
│     Statement → LogicalOperator tree                    │
│                                                          │
│  4. atlantis-cypher::planner::optimize(logical_plan)     │
│     LogicalOperator → LogicalOperator (optimized)        │
│                                                          │
│  5. atlantis-engine::executor::execute(plan, params)     │
│     Walks the LogicalOperator tree, evaluating against   │
│     the GraphStore and HnswIndex, producing Vec<Row>     │
│                                                          │
│  6. Convert rows → QueryResult { columns, rows }        │
│     → ResultSet { columns, rows }                        │
└─────────────────────────────────────────────────────────┘
```

The lexer tokenizes the query string, the parser builds an AST, the planner converts the AST into a logical operator tree, the optimizer applies transformation rules, and the executor walks the operator tree against the storage and vector index to produce result rows.

## Actor Model

Atlantis DB uses a single-writer actor model for concurrency safety:

```
                    ┌─────────────────────────────────┐
                    │        CoreEngineActor            │
                    │  (owns GraphStore + HnswIndex)     │
   EngineHandle ──►│                                   │
   (clonable,      │  Processes one Command at a time   │
    Arc-based)     │  → no data races, no locks         │
                    └──────────┬──────────────────────┘
                               │
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
        ┌──────────┐    ┌──────────┐    ┌──────────────┐
        │ Tiering  │    │Checkpoint│    │  Index       │
        │  Actor   │    │  Actor   │    │ Compaction   │
        │(future)  │    │(future)  │    │  Actor       │
        └──────────┘    └──────────┘    └──────────────┘
```

- **Core actor**: Owns the `GraphStore` and `HnswIndex`. Receives `Command` messages via a tokio `mpsc` channel and processes them sequentially. Because only one operation runs at a time, there are no data races and no locks needed.
- **Engine handle**: A cheaply clonable handle (`mpsc::Sender`) that callers share across async tasks. Each method sends a command and awaits a `oneshot` response.
- **Background actors**: Separate actors for tiering/eviction, checkpointing, and index compaction. In the current implementation, these are placeholders — tiering and checkpointing happen inline within the core actor. See [Implementation Roadmap](13-implementation-roadmap.md) for the plan.

See [Engine Runtime](08-engine-runtime.md) for the full actor design.

## Design Principles

### 1. Embedded First

Atlantis DB is a library, not a server. The user's application opens the database in-process, which eliminates network overhead and simplifies deployment. The `Engine` handle is cheaply clonable and safe to share across async tasks.

### 2. Single-Writer Actor Model

Rather than fine-grained locking, the core engine actor serializes all operations. This keeps the code simple and eliminates an entire class of concurrency bugs. Background tasks (tiering, checkpointing) run as separate actors that communicate via channels.

### 3. Hybrid Storage: Memory + Disk

Hot data stays in memory for sub-millisecond access. Cold data pages to disk. A configurable memory budget with LRU eviction moves data between tiers automatically. This lets datasets exceed available RAM without sacrificing hot-path performance. See [Storage Engine](04-storage-engine.md).

### 4. CSR + Delta for Adjacency

The hot tier uses Compressed Sparse Row (CSR) for cache-friendly read-optimized traversal, combined with a delta log for incremental writes. Periodic compaction merges the delta into the CSR. This avoids rebuilding the CSR on every edge insertion.

### 5. Vectors as First-Class Graph Citizens

Embeddings are stored on dedicated `:Embedding` nodes linked to content via relationships (e.g., `:EMBEDS`). This means vector search results are graph nodes you can traverse immediately — no separate vector store, no sync step. See [Vector Index](05-vector-index.md) and [RAG Patterns](11-rag-patterns.md).

### 6. Cypher Compatibility

The query language implements a practical subset of Cypher, the query language used by Neo4j and FalkorDB. Developers familiar with Cypher can adopt Atlantis DB with minimal friction. The parser is a hand-written recursive-descent parser with proper operator precedence. See [Cypher Query Language](06-cypher-query-language.md).

### 7. Zero-Cost Abstractions

Built entirely in Rust with zero-cost abstractions: `Arc<str>` for cheap label/relationship-type cloning, `Copy` for `NodeId`/`EdgeId`, contiguous vector storage for vectors and CSR arrays, and no garbage collection.

## Key Design Decisions

These decisions were made during the architecture phase and are documented in the [requirements spec](../.kiro/spec/atlantis-db/requirements.md):

| Decision | Choice | Rationale |
|---|---|---|
| Disk format | Append-only file with bincode serialization | Simple, durable, sufficient for v1. Page-based or LSM-tree is a future option. |
| Parser | Hand-written recursive descent | Full control over error messages and Cypher quirks. chumsky is listed as a dependency but the current parser is hand-written. |
| HNSW defaults | M=16, ef_construction=200, ef_search=50 | Based on the original HNSW paper recommendations. Tunable via config. |
| Adjacency representation | CSR + delta log | Read-optimized traversal (CSR) with efficient incremental updates (delta). Compaction threshold configurable. |
| Eviction policy | LRU (default) | Simple and effective. LFU available as an option in config. |
| Error handling | `thiserror` for library errors | Ergonomic typed errors with `#[error("...")]` attributes. `anyhow` reserved for binary/test code. |
| Serialization | serde + bincode | Compact binary format for persistence. All core types derive `Serialize`/`Deserialize`. |
| Logging | `tracing` crate | Structured, async-aware logging (available as a dependency; not yet wired throughout). |

## What's Next

- **[03 — Data Model](03-data-model.md)** — The property graph types that everything builds on.
- **[04 — Storage Engine](04-storage-engine.md)** — How data is stored in memory and on disk.
- **[08 — Engine Runtime](08-engine-runtime.md)** — The actor model in detail.
