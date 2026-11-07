# Atlantis DB Documentation

A next-generation embedded graph database engine for AI-native applications, built in Rust.

Atlantis DB unifies **graph traversal** and **vector similarity search** into a single query layer with a Cypher-compatible query language. It is designed from the ground up for AI agent memory, retrieval-augmented generation (RAG), and semantic knowledge graphs.

---

## Table of Contents

### Getting Started

- **[01 — Getting Started](01-getting-started.md)** — Installation, opening a database, running your first queries, and a quick vector search example.

### Architecture & Design

- **[02 — Architecture Overview](02-architecture.md)** — System-level architecture, crate dependency graph, data flow through the engine, and core design principles.
- **[03 — Data Model](03-data-model.md)** — The property graph model: `Value`, `Node`, `Relationship`, `Label`, `RelType`, `PropertyMap`, and the design decisions behind them.
- **[04 — Storage Engine](04-storage-engine.md)** — Hybrid tiered storage: in-memory hot tier with CSR + delta adjacency, on-disk cold tier, LRU eviction, and persistence.
- **[05 — Vector Index](05-vector-index.md)** — The HNSW algorithm, distance metrics, index construction, search, deletion, and parameter tuning.
- **[06 — Cypher Query Language](06-cypher-query-language.md)** — Supported Cypher subset, lexer, parser, AST structure, and vector extension functions.
- **[07 — Query Planner & Executor](07-query-planner-executor.md)** — Logical plan operators, planning from AST, execution model, and expression evaluation.
- **[08 — Engine Runtime](08-engine-runtime.md)** — Actor-based concurrency model, command protocol, handles, and background actors.

### API Reference

- **[09 — API Reference](09-api-reference.md)** — Complete reference for `Engine`, `Config`, `ResultSet`, `Row`, `Params`, `Value`, and error handling.

### Usage Guides

- **[10 — Query Cookbook](10-query-cookbook.md)** — Common Cypher query patterns: CRUD, filtering, ordering, pagination, and hybrid vector + graph queries.
- **[11 — RAG Patterns](11-rag-patterns.md)** — Building retrieval-augmented generation pipelines with embedding nodes, vector search, and graph traversal.
- **[12 — Configuration Reference](12-configuration.md)** — All configuration options: storage, vector index, HNSW parameters, and checkpointing.

### Implementation

- **[13 — Implementation Roadmap](13-implementation-roadmap.md)** — Current implementation status per crate, stubs and TODOs, remaining tasks, and testing status.

---

## How to Read This Documentation

**New to Atlantis DB?** Start with [Getting Started](01-getting-started.md), then read the [Architecture Overview](02-architecture.md) for the big picture.

**Want to build with it?** The [API Reference](09-api-reference.md), [Query Cookbook](10-query-cookbook.md), and [RAG Patterns](11-rag-patterns.md) are your primary resources. Use the [Configuration Reference](12-configuration.md) to tune the engine for your workload.

**Contributing or extending?** Read the per-crate design chapters ([03](03-data-model.md) through [08](08-engine-runtime.md)) to understand the internals, then check the [Implementation Roadmap](13-implementation-roadmap.md) for open work items.

---

## Project Repository

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
└── docs/                   # This documentation
```

Licensed under the Apache License, Version 2.0.
