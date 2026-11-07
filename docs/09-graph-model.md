# 09 — Graph Model

> **Status:** Partially implemented — single-node `MATCH`, node `CREATE`, and `DELETE` work; traversal, relationship creation, and variable-length paths are Planned — Phase 0; edge tables Phase 2; shortest paths and graph algorithms Phase 3
> **Crates:** `atlantis-cypher`, `atlantis-engine`, `atlantis-storage`, `atlantis-kv` (planned)

Atlantis DB is a property graph whose nodes and relationships are ordinary records.
A node is a record in the table named by its first label. A relationship is a record
in an edge table named by its type, with `in` and `out` record ids. This chapter
covers that mapping, edge schema, traversal, paths, and graph algorithms. The general
record model is in [03 — Data Model](03-data-model.md); the storage of adjacency is in
[04 — Storage Engine](04-storage-engine.md).

## Implemented Today

Verified against `crates/atlantis-cypher/src/parser/statement.rs`,
`crates/atlantis-cypher/src/planner/mod.rs`, and
`crates/atlantis-engine/src/executor.rs`. The workspace does not compile at the moment
(a missing import in `atlantis-core`); behaviour was checked on a copy with that import
added.

### Storage

- Nodes have a `NodeId` (`u64`), a label set, and a property map. Relationships have an
  `EdgeId`, `from`, `to`, one `RelType`, and properties.
- Adjacency is kept per relationship type as a CSR array plus a delta log of recent
  inserts. When a type's delta reaches `compaction_threshold` edges (default 1000), it
  is compacted into the CSR. Only **outgoing** adjacency exists.
- Deleting a node tombstones it. Neighbour lookups and label scans skip tombstoned
  nodes, but the node's edges are not removed.

### What works in Cypher

| Feature | Today |
|---|---|
| `MATCH (n)` and `MATCH (n:Label)` | Works. Scans all nodes or the first label. Extra labels are ignored. |
| `WHERE` on node properties | Works (see [06 — Query Language](06-query-language.md) for supported operators). |
| `CREATE (n:Label {...})` | Works for nodes. Returns only the last created node. |
| `DELETE n` | Works (tombstone). |
| `DETACH DELETE n` | Behaves like `DELETE`; edges stay. |
| Relationship patterns in `MATCH` | Broken. Only `-->` and `-[...]->` parse; `<-`, `-[...]-`, and `--` are parse errors. The expand step follows outgoing edges of the given type, and an untyped relationship returns no rows. The next node pattern then rescans its label and discards the expand rows, so in `(a)-[:KNOWS]->(b:Person)` the variable `b` binds to every `Person`, not to `a`'s neighbours. |
| Relationships in `CREATE` | Dropped by the planner (`CreateRelationship` is a TODO in the executor). The nodes are created; the edge is not. |
| Variable-length `*min..max` | `*`, `*2`, and `*..3` parse; `*1..3` is a lexer error (read as a float). The planner ignores the length and emits a single-hop expand. `VarLenExpand` is a TODO in the executor. |
| Path variables `p = (...)`, `shortestPath` | Not parsed. |

### Creating edges today

No public API creates a relationship. `Engine` (in `atlantis-db`) exposes `execute`,
`checkpoint`, and `shutdown`, and Cypher `CREATE` drops relationships. Edges can only
be inserted with `GraphStore::insert_edge` in `atlantis-storage`, below the engine.

## Nodes as Records (Planned — Phase 2)

| Graph concept | Record model |
|---|---|
| Node | A record |
| First label | The record's table. It is fixed for the life of the record. |
| Other labels | Secondary labels: an indexed tag set on the record |
| Node id | `RecordId`, for example `Person:alice`, returned by `id(n)` |
| Properties | The record's fields, including nested objects ([08](08-document-relational.md)) |

```cypher
CREATE (a:Person:Employee {id: 'alice', name: 'Alice'});
MATCH (p:Employee) RETURN id(p);      // Person:alice — scans the secondary label
MATCH (p:Person) SET p:Manager;       // adds a secondary label
```

A node created without a label (`CREATE ({note: 'x'})`) goes into the reserved
schemaless table `_default`; table names starting with `_` are reserved.

`SET n:Label` and `REMOVE n:Label` change secondary labels only. Removing or replacing
the first label is an error, because it would move the record to another table; create
a new record instead.

## Edges as Records (Planned — Phase 2)

Every relationship type is an **edge table**. An edge record has:

| Field | Meaning |
|---|---|
| `id` | The edge's `RecordId`, for example `WORKS_AT:01JC...` (ULID key by default) |
| `out` | `RecordId` of the start node; `startNode(r)` returns it |
| `in` | `RecordId` of the end node; `endNode(r)` returns it |
| other fields | The relationship's properties |

`type(r)` returns the edge table name. Edge tables are created implicitly, like node
tables, the first time an edge of that type is written. Declare one to add schema:

```cypher
CREATE TABLE WORKS_AT TYPE EDGE FROM Person TO Company;
CREATE FIELD role  ON WORKS_AT TYPE string;
CREATE FIELD since ON WORKS_AT TYPE option<datetime>;
CREATE INDEX works_since FOR ()-[w:WORKS_AT]-() ON (w.since);
CREATE CONSTRAINT employment_role FOR ()-[w:WORKS_AT]-() REQUIRE w.role IS NOT NULL;
```

`FROM` and `TO` are checked when an edge is written: an edge from a `Robot` to a
`Company` fails. Full DDL rules are in [07 — Schema & DDL](07-schema-ddl.md).

Because edges are records, they can be read like any other table:

```cypher
MATCH ()-[w:WORKS_AT]->() WHERE w.since < $cutoff RETURN w.role, count(*) AS n;
```

### Creating and deleting edges (Planned — Phase 0)

```cypher
MATCH (a:Person), (c:Company)
WHERE a.name = 'Alice' AND c.name = 'Atlantis'
CREATE (a)-[:WORKS_AT {role: 'engineer', since: time.now()}]->(c);

MATCH (a:Person)-[w:WORKS_AT]->(:Company) WHERE a.name = 'Alice' DELETE w;

MATCH (a:Person) WHERE a.name = 'Alice' DETACH DELETE a;   // deletes a and its edges
```

`DELETE` on a node that still has edges fails; use `DETACH DELETE`. This keeps every
`in` and `out` pointing at an existing record.

## Traversal (Planned — Phase 0)

Traversal uses standard Cypher patterns. All directions are supported:

| Pattern | Meaning |
|---|---|
| `(a)-[:KNOWS]->(b)` | Outgoing from `a` |
| `(a)<-[:KNOWS]-(b)` | Incoming to `a` |
| `(a)-[:KNOWS]-(b)` | Either direction (undirected match) |
| `(a)-[:KNOWS\|FOLLOWS]->(b)` | Any of several types |
| `(a)-->(b)`, `(a)-[r]->(b)` | Any type |

```cypher
MATCH (a:Person)-[:KNOWS]->(friend:Person)-[:WORKS_AT]->(c:Company)
WHERE a.name = $name
RETURN friend.name, c.name;
```

Incoming traversal costs the same as outgoing. Each edge is stored once as a record and
indexed twice in adjacency, under the start node with direction `out` and under the end
node with direction `in`, using the key prefix `.../g/{from}/{dir}/{edgeTable}/{edgeKey}`
([04 — Storage Engine](04-storage-engine.md)). An undirected pattern reads both.

An undirected pattern returns each matching edge once per direction it can be read in.
For `(a:Person)-[:KNOWS]-(b:Person)` that means both `(alice, bob)` and `(bob, alice)`,
as in Neo4j.

## Variable-Length Paths (Planned — Phase 0)

```cypher
MATCH (a:Person)-[:KNOWS*1..3]->(b:Person)
WHERE a.name = $name AND b <> a
RETURN DISTINCT b.name;
```

| Form | Hops |
|---|---|
| `*` | 1 or more |
| `*3` | Exactly 3 |
| `*2..4` | 2 to 4 |
| `*..4` | 1 to 4 |
| `*0..2` | 0 to 2 (0 binds `b` to `a`) |

Semantics follow Cypher: within one path, an edge is used at most once (trail
semantics), so cycles terminate. The relationship variable in a variable-length pattern
binds to the array of edges.

An unbounded `*` is accepted but can explore the whole connected component. Give an
upper bound in production queries; a bounded query runs within `query.timeout`
([22 — Configuration](22-configuration.md)) far more predictably.

## Paths and Path Functions (Planned — Phase 0)

A path variable binds the whole matched path:

```cypher
MATCH p = (a:Person)-[:KNOWS*1..3]->(b:Person)
WHERE a.name = 'Alice' AND b.name = 'Dan'
RETURN length(p) AS hops,
       [n IN nodes(p) | n.name] AS names,
       [r IN relationships(p) | type(r)] AS types;
```

| Function | Returns |
|---|---|
| `length(p)` | Number of edges |
| `nodes(p)` | Array of node records along the path |
| `relationships(p)` | Array of edge records along the path |
| `startNode(r)`, `endNode(r)` | The `out` and `in` record of an edge |
| `type(r)` | Edge table name |

## Shortest Paths (Planned — Phase 3)

```cypher
MATCH (a:Person), (b:Person)
WHERE a.name = 'Alice' AND b.name = 'Dan'
MATCH p = shortestPath((a)-[:KNOWS*..6]-(b))
RETURN [n IN nodes(p) | n.name] AS route;

MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Dan'})
MATCH p = allShortestPaths((a)-[:KNOWS*..6]-(b))
RETURN p;
```

`shortestPath` returns one path with the fewest edges; `allShortestPaths` returns every
path of that length. Both run a bidirectional breadth-first search from the two bound
endpoints. Predicates on the path's nodes and edges in `WHERE` are applied during the
search, not after it. Weighted shortest paths are an algorithm procedure, not a pattern.

## Graph Algorithms (Planned — Phase 3)

Whole-graph algorithms run as procedures in the `algo.*` namespace. Each takes a
configuration map that selects the subgraph by node table and edge table, and yields
one row per node:

```cypher
CALL algo.pagerank({nodes: 'Page', edges: 'LINKS_TO', iterations: 20, dampingFactor: 0.85})
YIELD node, score
RETURN node.url, score ORDER BY score DESC LIMIT 10;

CALL algo.wcc({nodes: 'Person', edges: 'KNOWS'})
YIELD node, componentId
RETURN componentId, count(*) AS size ORDER BY size DESC;
```

| Procedure | Yields | Purpose |
|---|---|---|
| `algo.pagerank(config)` | `node, score` | Node importance by incoming links |
| `algo.wcc(config)` | `node, componentId` | Weakly connected components |

- Algorithms read a consistent snapshot of the database
  ([05 — Transactions & Consistency](05-transactions-consistency.md)) and run on the
  in-memory adjacency cache.
- They return results; they do not write. To store a score, follow the `CALL` with
  `SET node.rank = score`.
- More algorithms (for example community detection and weighted shortest path) may be
  added in the same namespace and shape; only the two above are committed.

## Adjacency Cache and Performance Notes

Today's CSR + delta structure ([04 — Storage Engine](04-storage-engine.md)) stays in
the target design as an in-memory **adjacency cache**, rebuilt from the `g/` keys in
`atlantis-kv` (Phase 1). Practical consequences:

- **Typed patterns are faster.** Adjacency is grouped by edge table, so
  `-[:KNOWS]->` reads one contiguous range; an untyped `-->` reads every edge table of
  the node.
- **Hub nodes cost their degree.** Expanding a node with a million edges reads a
  million entries. Filter early with labels and edge types, and add an edge property
  index when you filter hubs' edges by a property.
- **Start from a selective node.** Anchor patterns with an indexed lookup
  (`WHERE a.email = $email`) or a record id, not with a label scan.
- **Write bursts accumulate in the delta.** The delta is compacted per edge table
  after `compaction_threshold` new edges; traversal stays correct before compaction,
  just slightly slower.
- **Bound variable-length patterns.** Cost grows roughly with the average degree to
  the power of the hop count.

## Design Decisions

**Why edges as records in their own tables?**
Edges then get everything records get: typed fields, indexes, constraints, permissions,
change feeds, and live queries, with no separate code path. Naming the table after the
relationship type keeps Cypher's `-[:TYPE]->` syntax unchanged.

**Why the first label as the table?**
A record needs exactly one home for schema and storage. Using the first label means
`CREATE (:Person {...})` needs no new syntax, while extra labels keep their Cypher role
as tags.

**Why store adjacency in both directions?**
Incoming traversal is as common as outgoing (followers, reverse dependencies). Two small
adjacency entries per edge make both directions equally cheap, without a scan.

**Why algorithms as procedures?**
`CALL ... YIELD` composes with the rest of the query, matches the vector and full-text
search procedures, and adds no grammar per algorithm.

## What's Next

- **[08 — Document & Relational Model](08-document-relational.md)** — Links versus edges, joins, and aggregation.
- **[10 — Vector Index](10-vector-index.md)** — Combining vector search with traversal.
- **[15 — Query Planner & Executor](15-query-planner-executor.md)** — Expand operators and the optimizer.
- **[23 — Query Cookbook](23-query-cookbook.md)** — Graph recipes.
