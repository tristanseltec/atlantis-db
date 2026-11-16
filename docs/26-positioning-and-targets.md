# 26 — Positioning & Performance Targets

> **Status:** Strategy. The baseline numbers are measured; every target is a goal, not a result. Reviewed 2026-11-16.
> **Crates:** `atlantis-vector`, `atlantis-text` (planned), `atlantis-engine`, `atlantis-cypher`, `atlantis-db`, `benches`

This chapter states what Atlantis DB competes on, who it competes with, and the numbers
it must reach to win. The [Architecture](02-architecture.md#hybrid-retrieval-path-focus)
chapter describes the system design that delivers them, and the
[Implementation Roadmap](25-implementation-roadmap.md#priorities) orders the work.

## The Killer Feature: Transactional Hybrid Retrieval

**Atlantis DB is the embedded Cypher database for agent memory and GraphRAG.** One
Cypher query, running inside the application process, does all of this:

1. a **filtered vector search**, where the filter (tenant, ACL, time window, type) is
   applied *during* the HNSW search, not after it;
2. a **BM25 full-text search**, fused with the vector hits by reciprocal rank fusion;
3. a **graph expansion** of one to three hops from the fused hits;
4. all on **one MVCC snapshot**, with **read-your-writes**: a memory an agent writes in a
   transaction can be retrieved in that same transaction.

```cypher
BEGIN;
CREATE (m:Memory {user: $user, text: $text, embedding: $vec, at: datetime()});
CALL db.search.hybrid({text: 'memory_text', vector: 'memory_vec'}, $q, $qvec, 10)
  YIELD node, score
  WHERE node.user = $user
MATCH (node)-[:ABOUT]->(e:Entity)-[:RELATED*1..2]->(ctx)
RETURN node.text, score, collect(DISTINCT ctx.name) AS context;
COMMIT;
```

### Why this, and not breadth

Agents write and read memory in tight loops, under per-user filters, and need graph
context around what they retrieve. Each property is a known weak point elsewhere:

- HNSW that filters *after* the search loses recall badly when the filter keeps 1% of
  the data or less, which is the normal case for per-user memory.
- Most stores refresh search indexes after commit, so an agent cannot read back what it
  just wrote in the same step.
- Stitching a vector store, a search engine and a graph database means three
  consistency models and three permission systems.

Breadth is not the place to win. SurrealDB already covers every model Atlantis plans,
from embedded to cluster, and has funding and a three-year lead. Atlantis wins by doing
one workload better than anyone, in the query language that GraphRAG tooling already
targets.

## Competitive Landscape

| Product | What it is | Strengths | Gap Atlantis targets |
|---|---|---|---|
| **SurrealDB 3.x** | Multi-model, Rust, embedded to cluster | 3.0 GA (Feb 2026); DiskANN (3.1); experimental GQL (3.2); faster traversals and filtered vector search (3.3) | Its own language, SurrealQL; graph support is still settling |
| **LadybugDB / RyuGraph** | Community forks of Kuzu (archived Oct 2025 after Apple acquired the company) | Embedded, Cypher, columnar, fast analytical scans; vector and full-text extensions | Community-maintained forks; search is an extension, not one engine path |
| **HelixDB** | Rust vector-graph database for AI apps | Vendor benchmarks claim 4–6x Neo4j on one-hop queries | Its own language, HelixQL; a server, not a library |
| **FalkorDB** | Redis module, Cypher, GraphBLAS | Fastest Cypher server on graph-only workloads (vendor numbers) | Needs a server; the vector index is an add-on |
| **Neo4j** | The incumbent graph database | Ecosystem and GraphRAG tooling | JVM server; heavy; slow in every published comparison |
| **LanceDB + lance-graph**, **sqlite-vec**, CozoDB forks, contextdb, CortexaDB, Grafeo | Embedded agent-memory stores | Small, single file | Partial or read-only Cypher, weak transactions, young projects |

The open position is an **embedded library that speaks standard Cypher and runs
filtered vector, full-text and graph retrieval together in one ACID snapshot**. Every
number quoted in this table is self-published by its vendor; the Atlantis benchmark
measures each competitor on the same hardware before any comparison is claimed.

## Scope Decisions

| Decision | Items |
|---|---|
| **Pull forward** | Filtered and persisted HNSW, read-your-writes search, BM25 and `db.search.hybrid`, vector and property pushdown in the planner, embedded Python package (PyO3) |
| **Keep** | RocksDB backend, schema and secondary indexes, change feeds and live queries, the server and auth |
| **Defer, unscheduled** | Time-series tables, spatial indexes, the key-value API, Bolt, Go and Java SDKs, the TiKV cluster (Phase 7) |

Deferred items keep their design chapters; they are not cancelled, only taken off the
critical path.

## The Benchmark: Atlantis Retrieval Bench

A public, reproducible harness in `benches/` (planned) that runs the same workload
against Atlantis and each competitor on the same machine.

| Parameter | Value |
|---|---|
| Corpus | 1,000,000 chunks, 768-dimension `f32` embeddings, 100,000 documents |
| Graph | 2,000,000 edges: chunk → document, chunk → entity, entity → entity |
| Filter | A `user` property with 1% selectivity (100 users) |
| Hybrid query | Filtered vector top-10, fused with BM25 top-10 by reciprocal rank fusion, then a 2-hop expansion, then `RETURN` |
| Mixed load | Queries on 8 threads while 2,000 new memories per second are written |
| Ground truth | Exact filtered k-NN, computed by brute force, for recall@10 |
| Reported | p50, p99 and p99.9 latency; QPS; recall@10; ingest rate; open time; resident memory |

Competitor adapters: SurrealDB (embedded), LadybugDB (embedded), FalkorDB, Neo4j, and
the stitched stack Qdrant + Neo4j as the reference for the multi-store approach.

## Performance Targets

| Metric | Target | Win condition |
|---|---|---|
| Hybrid query p50 / p99, embedded, 8 threads, under mixed load | **≤ 3 ms / ≤ 8 ms** | p99 at least 2x better than the best embedded competitor at equal recall |
| Filtered recall@10 at 1% selectivity | **≥ 0.95** | At or above every competitor |
| Freshness: from a write to being searchable | **0 — visible in the same transaction** | Better than any competitor whose search index refreshes after commit |
| Unfiltered vector top-10, 1M × 768 | **≤ 2 ms p50**, recall@10 ≥ 0.95 | On par with dedicated vector libraries |
| Durable ingest: record + embedding + edges, group commit | **≥ 50,000 records/s** | Above SurrealDB embedded |
| Open time, 1M-vector database | **< 1 s** | No index rebuild on open |
| Scan + filter + aggregate, 10k rows | **< 1 ms** | Credibility check, not the headline |

### Baseline (2026-11-16)

Short Criterion runs (`--warm-up-time 1 --measurement-time 3`) on a 12-core Linux
machine with 37 GB of RAM, in-memory engine. Mean times.

| Benchmark | Time | Reading |
|---|---|---|
| `record_get_by_id_10k` | 1.54 µs | Healthy |
| `neighbors_outgoing_10k` | 3.76 µs | Healthy |
| `record_insert_100_per_tx` | 143 µs (1.4 µs per record) | Healthy; not durable (no fsync) |
| `cypher_match_by_id` | 9.28 µs | Interpreter costs about 6x the raw lookup |
| `cypher_two_hop_traversal` | 23.8 µs | Fine |
| `cypher_scan_filter_aggregate_10k` | **39.8 ms** | About 4 µs per row; no planner, no batched execution |
| `vector_search_top10_5k_128d` | **562 µs** | 5–10x slower than comparable HNSW libraries at this size |

Single-record paths are ready. Vector search and scans are the two gaps, and the vector
gap is the main risk: if the HNSW path cannot reach about 2 ms at 1M × 768, the
headline target fails. That work comes first, after the harness.

### Retrieval Bench Baseline (2026-11-16)

First run of [`rbench`](../benches/retrieval/README.md) at the `small` scale (20,000
chunks × 768, 1% filter, 44,000 edges, k = 10), in-memory engine, same machine. Full
results: `benches/results/2026-11-16-small-mem.{json,md}`.

| Workload | p50 | p99 | Other | Target |
|---|---|---|---|---|
| Load | | | 110k items/s; **index build 85 s** | Open < 1 s at 1M |
| Vector top-10 | 0.28 ms | 1.44 ms | **recall@10 0.22** | recall ≥ 0.95 |
| Filtered top-10 (over-fetch 10×, then `WHERE`) | 0.51 ms | 2.73 ms | **recall@10 0.04**; every query short of 10 rows | recall ≥ 0.95 |
| Graph-RAG, 8 threads | 4.95 ms | 11.1 ms | 1,564 QPS | |
| Graph-RAG, 8 threads + writes | 10.1 ms | 19.7 ms | 789 QPS; **97 writes/s of 2,000** | p99 ≤ 8 ms at 1M, under load |
| Search reads own write | | | **0%** | 100% |

Findings:

1. **HNSW recall collapses with size — fixed.** Recall@10 was 1.00 at 5,000 × 768 but
   0.22 at 20,000 × 768, and raising `efSearch` did not help while latency fell: search
   visited fewer nodes as the index grew. Causes in `crates/atlantis-vector/src/hnsw.rs`:
   edges were stored undirected and pruning removed both directions, so nodes lost all
   links; neighbours were chosen without the diversity heuristic, which cut links
   between clusters; the insert-time descent compared neighbours with the current node
   instead of the new one, so it never moved; and search did not traverse deleted
   nodes, so a deleted entry point could return nothing. The fix (directed edges,
   heuristic selection with kept pruned connections, a correct descent, tombstones as
   waypoints) is pinned by `crates/atlantis-vector/tests/recall.rs` (clustered data:
   recall, reachability of every node, recall after 30% deletes). After the fix, the
   same run gives **recall@10 1.000** at p50 1.53 ms / p99 2.56 ms, and filtered recall
   0.09, the ceiling for filtering after a 10× over-fetch at 1% selectivity
   (`benches/results/2026-11-16-small-mem-hnsw-fix.*`). The early exit had made the old
   search look fast; latency and build time (now 177 s) are the next F2 work.
2. **Index build is about 4 ms per vector** at 768 dimensions, with cosine norms
   recomputed on every distance call.
3. **No concurrency.** Eight threads give 2–3x one thread, and a writer at 2,000/s
   achieves 97/s, consistent with the single mutex shared by vector search and
   index updates.
4. **Filtering after the search** returns fewer than k rows for every query at 1%
   selectivity; F3 removes it.

## What's Next

- [02 — Architecture](02-architecture.md#hybrid-retrieval-path-focus) — the design that
  delivers these targets.
- [25 — Implementation Roadmap](25-implementation-roadmap.md#priorities) — the order of
  work.
- [10 — Vector Index](10-vector-index.md) and [11 — Full-Text Search](11-full-text-search.md)
  — the index chapters this builds on.
