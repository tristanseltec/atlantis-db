# 24 — RAG Patterns

> **Status:** Partially implemented — vector + graph RAG (Patterns 1, 2 and 5, and the retrieval part of Pattern 3) works today; schema, hybrid search, agent-memory and tenancy features are marked Phase 2–5
> **Crates:** `atlantis-vector`, `atlantis-cypher`, `atlantis-engine`, `atlantis-storage`, `atlantis-text` (planned), `atlantis-index` (planned), `atlantis-auth` (planned)

Retrieval-augmented generation (RAG) and agent memory are the workloads Atlantis DB is
built for. One engine holds content, embeddings, relationships, text indexes and access
rules, so retrieval, expansion, citation and filtering run as one Atlantis Cypher query
instead of a pipeline across several stores.

Every pattern lists the phase it needs. The grammar for each construct is owned by its
chapter, linked inline.

## Implemented today

These pieces exist in the code and work together through `Engine::execute`:

- Named vector indexes: `CREATE VECTOR INDEX name FOR (n:Label) ON (n.prop) OPTIONS {...}`,
  derived from records and kept current after every commit
  ([10 — Vector Index](10-vector-index.md)).
- `CALL db.vector.search(indexOrLabel, $vec, k[, threshold | {threshold, efSearch}]) YIELD node, score`,
  where `node` is the hit's record id, so later `MATCH` clauses can start from it, and
  `score` is higher-is-better.
- Relationships, multi-hop and variable-length traversal, `OPTIONAL MATCH`, aggregation
  and record links that dereference (`node.doc.title`).

A search sees committed data only, so write the content in one statement or transaction
and search in a later one.

## Pattern 1: Document RAG (Works)

Store the embedding on its own node and link it to the content. The vector index stays
small and content can change without re-indexing.

```
(:Document {title, body, url}) <-[:EMBEDS]- (:Embedding {vector, model})
```

```rust
engine.execute(
    "CREATE VECTOR INDEX embedding_vec IF NOT EXISTS FOR (e:Embedding) ON (e.vector)
     OPTIONS {dimension: 384, metric: 'cosine'}",
    &[],
).await?;

engine.execute(
    "CREATE (e:Embedding {vector: $vec, model: $model})
            -[:EMBEDS]->(d:Document {title: $title, body: $body, url: $url})",
    &[("vec", embedding.into()), ("model", "all-MiniLM-L6-v2".into()),
      ("title", title.into()), ("body", body.into()), ("url", url.into())],
).await?;

let hits = engine.execute(
    "CALL db.vector.search('embedding_vec', $q, 10, 0.7) YIELD node, score
     MATCH (node)-[:EMBEDS]->(doc:Document)
     RETURN doc.title AS title, doc.body AS body, doc.url AS url, score
     ORDER BY score DESC",
    &[("q", query_embedding.into())],
).await?;
```

## Pattern 2: Chunked documents (Works)

Split long documents into chunks and embed each chunk.

```
(:Document)-[:HAS_CHUNK]->(:Chunk {index, text}) <-[:EMBEDS]- (:Embedding {vector})
```

```cypher
CALL db.vector.search('embedding_vec', $q, 5, 0.7) YIELD node, score
MATCH (node)-[:EMBEDS]->(c:Chunk)<-[:HAS_CHUNK]-(d:Document)
RETURN d.title AS title, c.text AS text, c.index AS chunkIndex, score
ORDER BY score DESC
```

Add neighbouring chunks for context with a second hop:
`MATCH (d)-[:HAS_CHUNK]->(n:Chunk) WHERE abs(n.index - c.index) <= 1`.

## Pattern 3: Chunk records with a vector index (index and retrieval work; schema Phase 2)

The embedding can live on the chunk record and be indexed by name, which replaces the
separate `:Embedding` node. The vector index and the retrieval query below work today;
the `SCHEMAFULL` tables, typed fields and `CREATE INDEX` are Phase 2. Grammar:
[07 — Schema and DDL](07-schema-ddl.md), [10 — Vector Index](10-vector-index.md).

```cypher
CREATE TABLE Doc SCHEMAFULL;
CREATE FIELD title ON Doc TYPE string;
CREATE FIELD source ON Doc TYPE string;
CREATE FIELD tenant ON Doc TYPE string;

CREATE TABLE Chunk SCHEMAFULL;
CREATE FIELD doc ON Chunk TYPE record<Doc>;
CREATE FIELD index ON Chunk TYPE int ASSERT value >= 0;
CREATE FIELD text ON Chunk TYPE string;
CREATE FIELD embedding ON Chunk TYPE vector<f32, 1536>;
CREATE FIELD lang ON Chunk TYPE option<string>;

CREATE TABLE HAS_CHUNK TYPE EDGE FROM Doc TO Chunk;

CREATE VECTOR INDEX chunk_embedding FOR (c:Chunk) ON (c.embedding)
  OPTIONS {dimension: 1536, metric: 'cosine', m: 16, efConstruction: 200};
CREATE INDEX chunk_doc FOR (c:Chunk) ON (c.doc);
```

With the Phase 2 schema, a write with the wrong dimension or a missing `text` fails with
`SchemaViolation`; today a vector of the wrong dimension is simply not indexed.
Retrieval uses the index name and the `doc` link (store `rid('Doc', ...)` in it today):

```cypher
CALL db.vector.search('chunk_embedding', $q, 20) YIELD node, score
WHERE node.lang = 'en'
RETURN node.doc.title AS title, node.text AS text, score
ORDER BY score DESC LIMIT 5
```

Several embedding models become several vector fields with one index each
(`embedding_small`, `embedding_large`). Field comparisons in the `WHERE` after `YIELD`
(`node.user = $user`, `node.lang IN $langs`, `node.createdAt > $since`) are applied inside
the index, so the search returns the `k` nearest matching chunks; other conditions filter
those `k` afterwards ([10 — Vector Index](10-vector-index.md)).

## Pattern 4: Hybrid text + vector retrieval (Phase 3)

Vector search misses exact terms (product codes, names, error strings). Full-text search
misses paraphrases. `db.search.hybrid` runs both and fuses the ranks with reciprocal
rank fusion (RRF). Grammar: [11 — Full-Text Search](11-full-text-search.md).

```cypher
CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];
CREATE FULLTEXT INDEX chunk_text FOR (c:Chunk) ON EACH [c.text]
  OPTIONS {analyzer: 'english'};

CALL db.search.hybrid({text: 'chunk_text', vector: 'chunk_embedding'},
                      $question, $q, 10) YIELD node, score
RETURN node.doc.title AS title,
       text.highlight(node.text, $question) AS snippet,
       score
ORDER BY score DESC
```

Pass both the raw question (for BM25) and its embedding (for HNSW). Retrieve wider than
you need, for example 50, then rerank outside the database if you use a cross-encoder.

## Pattern 5: Knowledge-graph RAG and citations (Works)

Graph edges turn "similar text" into "similar text plus its provenance". Record the
source of each chunk and the citations between documents. Return them with the answer
so the model can cite.

```
(:Doc)-[:CITES]->(:Doc)        (:Doc)-[:FROM_SOURCE]->(:Source {url, publisher})
```

```cypher
CALL db.vector.search('chunk_embedding', $q, 8) YIELD node, score
MATCH (d:Doc)-[:HAS_CHUNK]->(node)
OPTIONAL MATCH (d)-[:FROM_SOURCE]->(s:Source)
OPTIONAL MATCH (d)-[:CITES]->(cited:Doc)
RETURN node.text AS passage,
       d.title AS title,
       s.url AS sourceUrl,
       collect(DISTINCT cited.title) AS citations,
       score
ORDER BY score DESC
```

To expand entity neighbourhoods (GraphRAG style), extract entities into `(:Entity)`
nodes with `(:Chunk)-[:MENTIONS]->(:Entity)`. Then add
`MATCH (node)-[:MENTIONS]->(e:Entity)<-[:MENTIONS]-(related:Chunk)` to pull related
passages that are not lexically or semantically close.

## Pattern 6: Agent memory with TTL and live queries (Phases 3–4)

Agents need long-term memory (searchable), short-term scratch state (expiring) and
reactions to new facts.

```cypher
// Long-term memory: searchable records. Schema Phase 2; index and search work.
CREATE TABLE Memory SCHEMAFULL;
CREATE FIELD agent ON Memory TYPE record<Agent>;
CREATE FIELD text ON Memory TYPE string;
CREATE FIELD at ON Memory TYPE datetime DEFAULT time.now();
CREATE FIELD embedding ON Memory TYPE vector<f32, 384>;
CREATE VECTOR INDEX memory_embedding FOR (m:Memory) ON (m.embedding)
  OPTIONS {dimension: 384, metric: 'cosine'};

CALL db.vector.search('memory_embedding', $q, 20) YIELD node, score
WHERE node.agent = $agentId
RETURN node.text, node.at, score ORDER BY score DESC LIMIT 5
```

```cypher
// Short-term working memory: expires on its own.           Phase 3
CALL db.kv.set('agent:' + $agentId + ':scratch', $state, {ttl: '30m'})
```

```cypher
// React to new memories (for example to summarise).        Phase 4
LIVE MATCH (m:Memory) WHERE m.agent = $agentId RETURN m;
```

```rust
let mut feed = engine.live(
    "MATCH (m:Memory) WHERE m.agent = $agent RETURN m",
    &[("agent", agent_id.into())],
).await?;
while let Some(n) = feed.next().await { summarise(n?.record).await?; }
```

For time-bounded recall, filter on `at`, for example
`node.at >= time.now() - duration('P7D')`. For durable memory that should still expire,
set a time-series `RETENTION` on the table
([13 — Key-Value and Time-Series](13-key-value-time-series.md)).

## Pattern 7: Multi-tenant RAG (Phase 2 tenancy; Phase 5 permissions)

There are two isolation levels. Choose per product.

**Namespace or database per tenant** (hard isolation, Phase 2). Each tenant gets
`USE <tenant>.kb;` with its own tables and indexes. Nothing is shared, and dropping a
tenant drops its database.

```cypher
CREATE NAMESPACE acme;
CREATE DATABASE acme.kb;
USE acme.kb;
```

**Shared tables with record permissions** (soft isolation, Phase 5). Tenants share
indexes, and permissions filter every read, including results from
`db.vector.search` and `db.search.hybrid`. Grammar:
[18 — Security and Auth](18-security-auth.md).

```cypher
ALTER TABLE Doc PERMISSIONS
  FOR read WHERE d.tenant = $auth.tenant
  FOR create, update, delete WHERE d.tenant = $auth.tenant;
ALTER TABLE Chunk PERMISSIONS
  FOR read WHERE c.doc.tenant = $auth.tenant;
```

With shared indexes, filtering happens after the approximate search. Ask for a larger
`k` than you return so that each tenant still gets enough rows. Use a database per
tenant when tenants are large or when compliance needs physical separation.

## Best Practices

### Embedding layout

- Put the vector on the chunk record with a named vector index (Pattern 3), or on a
  dedicated `:Embedding` node linked with `EMBEDS` when several models or versions
  coexist. From Phase 2 the field can be typed `vector<f32, N>`.
- Store model name, chunk index and timestamps beside the vector so results can be
  filtered and audited.

### Dimensions and metric

The dimension must equal the embedding model's output. Set it with `dimension` in each
vector index (the default is `VectorConfig.dimensions`). A query vector of another
length fails with `DimensionMismatch`; a stored vector of another length is not indexed.

| Metric | Use for |
|---|---|
| `Cosine` (default) | Most text embedding models. |
| `Euclidean` | Spatial data or models trained for L2 distance. |
| `DotProduct` | Pre-normalised vectors or maximum inner product models. |

### Thresholds

The optional fourth argument of `db.vector.search` keeps results whose score is at least
the threshold (cosine similarity for `cosine`). Start at 0.7 for cosine. Raise it for precision and lower
it, or omit it, for recall. Thresholds are model-specific, so recalibrate when you
change models.

### Recall tuning

For RAG, recall usually matters more than latency. Raise `m` and `efConstruction` in
the index options and `efSearch` at query time (`{efSearch: 200}`). Presets are in
[22 — Configuration](22-configuration.md#tuning-presets).

## Design Decisions

- **Retrieval, expansion and filtering in one query.** Keeping vectors, text and edges
  in one engine avoids sync jobs and makes the graph part of the result consistent with
  a single snapshot. Vector hits come from committed data.
- **Hybrid fusion by rank, not score.** BM25 and cosine scores are not comparable. RRF
  needs no calibration, which suits a default.
- **Permissions apply after index search.** Shared-index tenancy stays simple and
  correct, at the cost of over-fetching. Database-per-tenant is the answer when that
  cost matters.

## What's Next

- [23 — Query Cookbook](23-query-cookbook.md) — the individual recipes used here.
- [10 — Vector Index](10-vector-index.md) — HNSW internals and index DDL.
- [11 — Full-Text Search](11-full-text-search.md) — analyzers, BM25 and RRF.
