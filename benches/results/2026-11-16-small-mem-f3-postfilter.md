## Retrieval Bench — atlantis (embedded, in-memory) — small

20000 chunks × 768d, 100 users (filter selectivity 1.00%), 44000 edges, 300 queries, k = 10. linux x86_64 with 12 CPUs; commit 7271661; 2026-11-16.

Capabilities: filter in search no, BM25 hybrid no, search reads own writes no, graph in query yes, embedded yes. Notes: filtered search = vector top-10×10 then WHERE; efSearch default; no BM25 (graph-RAG is vector-only).

| Workload | p50 ms | p99 ms | Mean ms | Other |
|---|---|---|---|---|
| Load | | | | 67000 items in 0.8s (79722/s); index build 5.38s |
| Vector top-k | 0.378 | 0.624 | 0.390 | recall@k 1.000 |
| Filtered vector top-k | 1.113 | 2.489 | 1.188 | recall@k 0.092; 300 of 300 queries short |
| Graph-RAG, 1 thread | 1.096 | 2.440 | 1.200 |  |
| Graph-RAG, 8 threads | 2.081 | 5.092 | 2.216 | 3610 QPS; 0 errors |
| Graph-RAG, 8 threads + writes | 3.429 | 9.729 | 3.750 | 2133 QPS; writes 261/s of 2000 target |
| Write (mixed) | 3.379 | 9.644 | 3.823 | 0 errors |
