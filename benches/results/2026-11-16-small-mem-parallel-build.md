## Retrieval Bench — atlantis (embedded, in-memory) — small

20000 chunks × 768d, 100 users (filter selectivity 1.00%), 44000 edges, 300 queries, k = 10. linux x86_64 with 12 CPUs; commit 4c1dd9e; 2026-11-16.

Capabilities: filter in search no, BM25 hybrid no, search reads own writes no, graph in query yes, embedded yes. Notes: filtered search = vector top-10×10 then WHERE; efSearch default; no BM25 (graph-RAG is vector-only).

| Workload | p50 ms | p99 ms | Mean ms | Other |
|---|---|---|---|---|
| Load | | | | 67000 items in 0.6s (107439/s); index build 4.96s |
| Vector top-k | 0.264 | 0.455 | 0.274 | recall@k 1.000 |
| Filtered vector top-k | 0.910 | 1.765 | 0.993 | recall@k 0.092; 300 of 300 queries short |
| Graph-RAG, 8 threads | 1.875 | 3.205 | 1.919 | 4168 QPS; 0 errors |
| Graph-RAG, 8 threads + writes | 2.792 | 5.419 | 2.931 | 2729 QPS; writes 341/s of 2000 target |
| Write (mixed) | 2.760 | 5.148 | 2.934 | 0 errors |
