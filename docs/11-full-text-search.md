# 11 — Full-Text Search

> **Status:** Planned — Phase 3
> **Crates:** `atlantis-text` (planned), `atlantis-kv` (planned), `atlantis-cypher`, `atlantis-engine`

Full-text search finds records by the words they contain and ranks them by relevance.
Atlantis DB has no full-text support today: nothing in `crates/` tokenizes text or
builds an inverted index, and string matching is limited to the operators described in
[06 — Query Language](06-query-language.md). Everything below is the Phase 3 target
design.

## Overview

```
 text field ──► analyzer ──► terms ──► inverted index (ft/ keys in atlantis-kv)
                (tokenizer +            term → postings (record key, frequency, positions)
                 filters)
                                                     │
 query string ──► same analyzer ──► terms ──► BM25 scoring ──► top-k records
```

| Concept | Meaning |
|---|---|
| **Analyzer** | A named pipeline: one tokenizer followed by zero or more filters |
| **Tokenizer** | Splits text into tokens |
| **Filter** | Rewrites, drops or expands tokens |
| **Full-text index** | An inverted index over one or more string fields of one table |
| **BM25** | The ranking function used for relevance scores |

## Analyzers (Planned — Phase 3)

### Defining an Analyzer

```cypher
CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];
CREATE ANALYZER autocomplete TOKENIZER unicode FILTERS [lowercase, edgengram(2, 10)];
CREATE ANALYZER code TOKENIZER camel FILTERS [lowercase];
DROP ANALYZER code;
```

Analyzers are database-level schema objects, stored in the catalog under `/sys/...`.
An analyzer cannot be dropped while an index uses it. Two analyzers exist without being
created: `standard` (`unicode` + `lowercase`, the default) and `keyword` (the whole value
becomes one token, for exact matching of tags and codes).

### Tokenizers

| Tokenizer | Splits on |
|---|---|
| `unicode` | Unicode word boundaries (UAX #29). The right choice for natural language. |
| `whitespace` | Whitespace only. Punctuation stays attached to tokens. |
| `punct` | Whitespace and punctuation. |
| `camel` | Whitespace, punctuation and case changes (`parseHttpRequest` → `parse`, `Http`, `Request`). |
| `keyword` | Does not split. |

### Filters

| Filter | Effect |
|---|---|
| `lowercase` | Lowercases tokens using Unicode case folding |
| `ascii` | Folds accented characters to ASCII (`Huế` → `Hue`) |
| `stopwords('english')` | Drops common words for a language |
| `snowball('english')` | Snowball stemming (`running` → `run`) for a supported language |
| `edgengram(min, max)` | Emits prefixes of each token, for search-as-you-type |
| `ngram(min, max)` | Emits all substrings in the length range, for infix matching |

Filters run in the declared order. Put `lowercase` and `ascii` before `snowball` so the
stemmer sees normalized input.

### Testing an Analyzer

```cypher
CALL db.text.analyze('english', 'Running graph databases in Huế') YIELD token
RETURN collect(token);
// ['run', 'graph', 'databas', 'in', 'hue']
```

## Full-Text Index DDL (Planned — Phase 3)

The index form follows Neo4j 5:

```cypher
CREATE FULLTEXT INDEX doc_text FOR (d:Doc) ON EACH [d.title, d.body]
  OPTIONS {analyzer: 'english'};

CREATE FULLTEXT INDEX doc_text_tuned FOR (d:Doc) ON EACH [d.title, d.body]
  OPTIONS {analyzer: 'english', k1: 1.4, b: 0.6, highlight: true};

DROP INDEX doc_text_tuned;
```

| Option | Default | Meaning |
|---|---|---|
| `analyzer` | `'standard'` | Analyzer used for both indexing and queries |
| `k1` | `1.2` | BM25 term-frequency saturation |
| `b` | `0.75` | BM25 document-length normalization |
| `highlight` | `true` | Store token positions; required for phrase queries and `text.highlight` |

- An index covers one table (the node's first label) and one or more fields. Each field
  is indexed separately, so a query can target a single field.
- Fields of type `string` are indexed directly. Arrays of strings index each element.
  Other values are ignored.
- Building an index on existing data runs in the background through the index manager
  (see [07 — Schema & DDL](07-schema-ddl.md)). After that, index updates commit in the
  same transaction as the record write, so search results are never ahead of or behind
  the data in the reader's snapshot.
- Edge tables can carry full-text indexes too: `FOR ()-[r:REVIEWED]-() ON EACH [r.comment]`.

## Searching (Planned — Phase 3)

```cypher
CALL db.text.search('doc_text', 'graph database', 10) YIELD node, score
RETURN node.title, score
ORDER BY score DESC;
```

`db.text.search(index, query, k)` returns at most `k` records ordered by BM25 score,
highest first. Like `db.vector.search`, an optional fourth argument takes an options
map, and later clauses can filter:

```cypher
CALL db.text.search('doc_text', $q, 20, {fields: ['title']}) YIELD node AS d, score
WHERE d.published = true
RETURN d, score LIMIT 5;
```

### Query String Syntax

The query string is analyzed with the index analyzer, then interpreted as follows.

| Syntax | Meaning | Example |
|---|---|---|
| `a b` | Any term may match; more matches rank higher | `graph database` |
| `+a` / `-a` | Term is required / excluded | `+rust -java` |
| `a AND b`, `a OR b`, `NOT a` | Boolean operators (uppercase); parentheses group | `(graph OR vector) AND rust` |
| `"a b"` | Phrase: terms adjacent and in order | `"graph database"` |
| `"a b"~3` | Proximity: terms within 3 positions | `"vector index"~3` |
| `data*` | Prefix match | `embed*` |
| `term~` / `term~2` | Fuzzy match within edit distance 1 (default) or 2 | `databse~` |
| `field:term` | Restrict to one indexed field | `title:atlantis` |
| `term^2` | Boost a clause | `title:graph^2 body:graph` |

Prefix and fuzzy terms expand to the matching dictionary terms, capped at a fixed
expansion limit to keep queries bounded. They are not stemmed. A query string with a
syntax error fails with `AtlantisError::InvalidQuery` and the column of the error.

To match a user's raw input without interpreting operators, pass it through
`text.escape($input)` first.

## BM25 Scoring (Planned — Phase 3)

For a query with terms `q₁..qₙ` and a record field `D`:

```
score(D) = Σ  IDF(qᵢ) · f(qᵢ, D) · (k1 + 1)
           i  ─────────────────────────────────────────
              f(qᵢ, D) + k1 · (1 - b + b · |D| / avgdl)

IDF(qᵢ) = ln(1 + (N - n(qᵢ) + 0.5) / (n(qᵢ) + 0.5))
```

`f` is the term frequency in the field, `|D|` the field length in tokens, `avgdl` the
average field length, `N` the number of indexed records and `n(qᵢ)` the number of
records containing the term. With several fields, the record score is the sum of the
field scores, each multiplied by its boost.

- `k1` controls how fast repeated terms stop adding score. Lower it for short fields.
- `b` controls how strongly long fields are penalized. Set `b: 0` for titles, tags and
  other fields whose length carries no meaning.

Statistics (`N`, `avgdl`, document frequencies) are read from the transaction's
snapshot, so a score is reproducible within a transaction.

## Highlighting (Planned — Phase 3)

```cypher
CALL db.text.search('doc_text', 'graph database', 5) YIELD node AS d, score
RETURN d.title,
       text.highlight('doc_text', d.body, 'graph database',
                      {pre: '<mark>', post: '</mark>', fragmentSize: 160}) AS snippet;
```

`text.highlight(index, value, query, options)` re-analyzes `value` with the index's
analyzer and wraps every token that matches the query. With `fragmentSize`, it returns
the highest-scoring fragment instead of the whole text. `pre` and `post` default to
`<b>` and `</b>`. It works on any string, but uses stored positions when `value` is an
indexed field of the record being returned.

## Hybrid Search with Reciprocal Rank Fusion

> Planned — Phase 3. The vector side (`CREATE VECTOR INDEX`, `db.vector.search`) is implemented; see [10 — Vector Index](10-vector-index.md).

Keyword search is precise for names, codes and rare words; vector search finds
paraphrases. Hybrid search runs both and fuses the ranked lists.

```cypher
CALL db.search.hybrid({text: 'doc_text', vector: 'chunk_embedding'},
                      'graph database', $vec, 10) YIELD node, score   // RRF
RETURN node.title, score;
```

Both indexes must cover the same table. Each side retrieves a candidate list, and every
record gets a fused score with **Reciprocal Rank Fusion**:

```
rrf(d) = Σ  wᵢ / (rrfK + rankᵢ(d))
         i ∈ {text, vector}
```

`rankᵢ(d)` is the 1-based position of `d` in list `i`. A record missing from a list
contributes nothing for that list.

An optional fifth argument tunes the fusion:

```cypher
CALL db.search.hybrid({text: 'doc_text', vector: 'chunk_embedding'},
                      $q, $vec, 10,
                      {rrfK: 60, weights: {text: 1.0, vector: 2.0}, candidates: 100})
YIELD node, score
RETURN node, score;
```

| Option | Default | Meaning |
|---|---|---|
| `rrfK` | `60` | Damping constant. Higher values flatten the advantage of top ranks. |
| `weights` | `{text: 1.0, vector: 1.0}` | Per-list weight `wᵢ` |
| `candidates` | `max(4 × k, 50)` | Size of each candidate list before fusion |

`WHERE` after `YIELD` filters the fused result. When a predicate can be pushed down,
the planner applies it to both candidate lists before fusion, so that `k` matching
results survive.

## Storage Layout (Planned — Phase 3)

Postings live in `atlantis-kv` under the table's `ft/` prefix
(see [04 — Storage Engine](04-storage-engine.md) for the full key encoding):

```
.../tb/{tb}/ft/{index}/t/{field}/{term}/{key}   -> term frequency + positions
.../tb/{tb}/ft/{index}/d/{key}                  -> per-field lengths + term list
.../tb/{tb}/ft/{index}/s/{shard}                -> partial statistics (N, total length)
.../tb/{tb}/ft/{index}/df/{field}/{term}        -> document frequency
```

- `t/` keys are ordered by term, so prefix (`embed*`) queries are range scans, and
  fuzzy queries walk the term dictionary with a Levenshtein automaton.
- `d/` records the terms of each record, so an update or delete removes exactly the
  old postings without re-analyzing the old value.
- Statistics are split over several `s/{shard}` keys and summed on read, so concurrent
  writers do not all conflict on one counter key.
- All postings for a record change in the record's own transaction.

`atlantis-text` owns analyzers, the query-string parser, BM25 and the posting format.
The engine calls it during writes and the executor calls it for
`db.text.search`. In a cluster, the postings are shared through KV and each node
caches hot terms (see [20 — Distributed Architecture](20-distributed-architecture.md)).

## Design Decisions

**Why BM25?** It is the established baseline for lexical relevance, needs no training,
and has two intuitive parameters.

**Why postings in the transactional KV instead of a separate search engine?** Results
stay consistent with the data in the same snapshot, backups and replication cover the
index automatically, and there is no second system to operate.

**Why Reciprocal Rank Fusion?** BM25 scores and vector similarities are on different,
query-dependent scales. RRF only uses ranks, so it needs no score normalization and
behaves predictably across queries.

**Why a Lucene-like query string?** It is familiar to most developers, and Neo4j's
full-text procedures use a similar syntax, which eases migration.

## What's Next

- [10 — Vector Index](10-vector-index.md) — the vector side of hybrid search.
- [07 — Schema & DDL](07-schema-ddl.md) — index lifecycle and background builds.
- [24 — RAG Patterns](24-rag-patterns.md) — hybrid retrieval in RAG pipelines.
- [25 — Implementation Roadmap](25-implementation-roadmap.md) — Phase 3 scope.
