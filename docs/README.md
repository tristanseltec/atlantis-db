# Atlantis DB Documentation

> **Status:** Partially implemented — this index covers implemented chapters and target-design chapters; each chapter declares its own status
> **Crates:** all workspace crates; see [Architecture](02-architecture.md)

Atlantis DB is a multi-model database written in Rust. Graph, vector, document,
relational, key-value, time-series, full-text and geospatial data share one record
model and one query language, **Atlantis Cypher** (Cypher plus extensions). It is
designed to run embedded, as a standalone server and as a distributed cluster.

Today the code is a pre-release embedded engine with one record store: documents, graph
nodes and edges in namespaced tables, with MVCC transactions and a write-ahead log.
Cypher and the Rust record API read and write the same records, and vector indexes are
derived from them (what works is listed in [Getting Started](01-getting-started.md)).
Most of this documentation
describes the target design, and every chapter separates **Implemented today** from
**Planned — Phase N**. The phases are defined in the
[Implementation Roadmap](25-implementation-roadmap.md).

## Chapters

Status values: `Implemented`, `Partial` (some of the chapter is implemented), or the
phase that delivers it.

### Part I — Foundations

| # | Chapter | What it covers | Status |
|---|---|---|---|
| 01 | [Getting Started](01-getting-started.md) | Embedded quickstart, server quickstart, a tour of every model | Partial |
| 02 | [Architecture](02-architecture.md) | Layers, crate map, deployment modes, request flow | Partial |
| 03 | [Data Model](03-data-model.md) | Records, namespaces, databases, tables, RecordId, value types | Partial |

### Part II — Storage and Transactions

| # | Chapter | What it covers | Status |
|---|---|---|---|
| 04 | [Storage Engine](04-storage-engine.md) | Transactional KV layer, key encoding, write-ahead log, backends, record store, adjacency cache | Partial |
| 05 | [Transactions & Consistency](05-transactions-consistency.md) | ACID, MVCC, isolation levels, explicit transactions, durability, conflicts | Partial |

### Part III — Query Language and Schema

| # | Chapter | What it covers | Status |
|---|---|---|---|
| 06 | [Query Language](06-query-language.md) | Atlantis Cypher: today's subset and the extension catalogue | Partial |
| 07 | [Schema & DDL](07-schema-ddl.md) | Namespaces, databases, tables, fields, types, constraints, index DDL | Partial (vector index DDL implemented; rest Phase 2) |
| 15 | [Query Planner & Executor](15-query-planner-executor.md) | Today's clause interpreter; planner, cost-based optimizer, joins, index selection | Partial |

### Part IV — Data Models

| # | Chapter | What it covers | Status |
|---|---|---|---|
| 08 | [Document & Relational](08-document-relational.md) | Nested documents, record links, joins, aggregation, secondary indexes | Partial |
| 09 | [Graph Model](09-graph-model.md) | Edges as records, traversal, paths, shortest path, graph algorithms | Partial |
| 10 | [Vector Index](10-vector-index.md) | HNSW, vector index DDL and search today; persistence, filtering, quantization, hybrid search | Partial |
| 11 | [Full-Text Search](11-full-text-search.md) | Analyzers, BM25, full-text index DDL, highlighting, text + vector fusion | Phase 3 |
| 12 | [Geospatial](12-geospatial.md) | Geometry types, geo functions, spatial index | Partial (`point`, `point.distance`; rest Phase 3) |
| 13 | [Key-Value & Time-Series](13-key-value-time-series.md) | KV API, TTL, range scans; time-series tables, retention, downsampling | Phase 3 |
| 14 | [Live Queries & Events](14-live-queries-events.md) | Change feeds, `LIVE` queries, events and triggers | Phase 4 |

### Part V — Runtime and Platform

| # | Chapter | What it covers | Status |
|---|---|---|---|
| 16 | [Engine Runtime](16-engine-runtime.md) | Today's datastore runtime and its limits; commit sequencer, query workers, background workers | Partial |
| 17 | [Server & Protocols](17-server-protocols.md) | `atlantis` server and CLI, HTTP, WebSocket RPC, Bolt, import/export, backup | Phase 5 |
| 18 | [Security & Auth](18-security-auth.md) | System users, roles, access methods, JWT, record-level permissions, audit | Phase 5 |
| 19 | [Client SDKs](19-client-sdks.md) | Connection URLs; Rust, Python, JavaScript/WASM, Go and Java SDKs | Partial (remote SDKs: Phase 6) |
| 20 | [Distributed Architecture](20-distributed-architecture.md) | Compute/storage separation, TiKV, cluster membership, distributed live queries | Phase 7 |

### Part VI — Reference and Guides

| # | Chapter | What it covers | Status |
|---|---|---|---|
| 21 | [API Reference](21-api-reference.md) | Rust API today; planned API surface | Partial |
| 22 | [Configuration](22-configuration.md) | Current options; planned storage, server, auth and cluster settings | Partial |
| 23 | [Query Cookbook](23-query-cookbook.md) | Recipes for each model | Partial |
| 24 | [RAG Patterns](24-rag-patterns.md) | Retrieval pipelines, hybrid search, document chunks, agent memory | Partial |

### Part VII — Project

| # | Chapter | What it covers | Status |
|---|---|---|---|
| 25 | [Implementation Roadmap](25-implementation-roadmap.md) | Current status per crate, the focus-track priorities and the phased plan to multi-model | — |
| 26 | [Positioning & Performance Targets](26-positioning-and-targets.md) | The killer feature, competitors, scope decisions, the retrieval benchmark, targets and today's baseline | Strategy |
| 27 | [Commerce](27-commerce.md) | The first vertical: B2B and B2C, humans and agents; reference model, agent protocols, engine requirements, targets, stages | Strategy (C0–C4) |

## Reading Paths

**Evaluating Atlantis DB.** [Getting Started](01-getting-started.md) →
[Positioning & Performance Targets](26-positioning-and-targets.md) →
[Architecture](02-architecture.md) → [Implementation Roadmap](25-implementation-roadmap.md).
This shows what runs today and what is still a design.

**Building with today's embedded engine.** [Getting Started](01-getting-started.md) →
[API Reference](21-api-reference.md) → [Query Language](06-query-language.md) →
[Configuration](22-configuration.md) → [Query Cookbook](23-query-cookbook.md). Read the
"Implemented today" sections and skip the planned ones.

**Building AI retrieval and agent memory.** [Vector Index](10-vector-index.md) →
[Full-Text Search](11-full-text-search.md) → [RAG Patterns](24-rag-patterns.md) →
[Live Queries & Events](14-live-queries-events.md).

**Modelling application data.** [Data Model](03-data-model.md) →
[Schema & DDL](07-schema-ddl.md) → [Document & Relational](08-document-relational.md) →
[Graph Model](09-graph-model.md) → [Key-Value & Time-Series](13-key-value-time-series.md).

**Running it as a service.** [Server & Protocols](17-server-protocols.md) →
[Security & Auth](18-security-auth.md) → [Client SDKs](19-client-sdks.md) →
[Distributed Architecture](20-distributed-architecture.md).

**Contributing to the engine.** [Architecture](02-architecture.md) →
[Data Model](03-data-model.md) → [Storage Engine](04-storage-engine.md) →
[Transactions & Consistency](05-transactions-consistency.md) →
[Query Planner & Executor](15-query-planner-executor.md) →
[Engine Runtime](16-engine-runtime.md) → [Implementation Roadmap](25-implementation-roadmap.md).

## Conventions

- Every chapter opens with a **Status** line and the **Crates** it covers. Crates that
  do not exist yet are marked "(planned)".
- Planned sections carry their phase in the heading, for example
  `## Secondary Indexes (Planned — Phase 2)`. Tables use a Status column.
- Query examples use Atlantis Cypher. Syntax that standard Cypher already defines is
  used unchanged; extensions are documented in [Query Language](06-query-language.md)
  and the chapter that owns each feature.
