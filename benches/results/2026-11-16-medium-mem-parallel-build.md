## Retrieval Bench — atlantis (embedded, in-memory) — medium

100000 chunks × 768d, 100 users (filter selectivity 1.00%), 220000 edges, 500 queries, k = 10. linux x86_64 with 12 CPUs; commit 4c1dd9e; 2026-11-16.

Capabilities: filter in search no, BM25 hybrid no, search reads own writes no, graph in query yes, embedded yes. Notes: filtered search = vector top-10×10 then WHERE; efSearch default; no BM25 (graph-RAG is vector-only).

| Workload | p50 ms | p99 ms | Mean ms | Other |
|---|---|---|---|---|
| Load | | | | 335000 items in 4.2s (79694/s); index build 27.25s |
| Vector top-k | 0.236 | 0.535 | 0.252 | recall@k 1.000 |
| Graph-RAG, 8 threads | 1.541 | 3.125 | 1.594 | 5017 QPS; 0 errors |
