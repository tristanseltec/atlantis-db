# 10 — Query Cookbook

A collection of common Cypher query patterns for Atlantis DB. Each example includes the query, a description, and notes on current implementation status.

## Creating Data

### Create a Node

```cypher
CREATE (n:Person {name: 'Alice', age: 30}) RETURN n
```

Creates a `:Person` node with two properties. The `RETURN n` returns the created node's ID.

### Create Multiple Nodes

```cypher
CREATE (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'})
```

Creates two nodes in a single query. (Note: multi-element CREATE patterns are parsed; each node element becomes a separate `CreateNode` operator.)

### Create a Relationship

```cypher
CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})
```

Creates two nodes and a `:KNOWS` relationship between them. **Note**: In the current implementation, node creation works but relationship creation in CREATE is a TODO — the executor creates nodes but does not yet wire the relationships. See [Implementation Roadmap](13-implementation-roadmap.md).

### Create with Parameters

```cypher
CREATE (n:Person {name: $name, age: $age}) RETURN n
```

```rust
engine.execute(
    "CREATE (n:Person {name: $name, age: $age}) RETURN n",
    &[("name", "Alice".into()), ("age", 30i64.into())],
).await?;
```

## Reading Data

### Match All Nodes

```cypher
MATCH (n) RETURN n
```

Scans all nodes. Produces a row per node with the node ID bound to `n`.

### Match by Label

```cypher
MATCH (n:Person) RETURN n
```

Uses the label index to find all `:Person` nodes efficiently.

### Match with Property Filter

```cypher
MATCH (n:Person) WHERE n.name = 'Alice' RETURN n
```

Scans `:Person` nodes, then filters by the `name` property. The `WHERE` clause evaluates the property access `n.name` by looking up the node by ID and reading the property.

### Match a Relationship Pattern

```cypher
MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a, b
```

Finds all `:KNOWS` relationships between `:Person` nodes. The planner scans `:Person` nodes, then expands via the `:KNOWS` relationship type.

### Match with Multiple Filters

```cypher
MATCH (n:Person) WHERE n.name = 'Alice' AND n.age > 25 RETURN n.name, n.age
```

Combines filters with `AND`. Also supports `OR`, `NOT`, and `XOR`.

### Match with Comparison Operators

```cypher
MATCH (n:Person) WHERE n.age >= 18 AND n.age <= 65 RETURN n.name
```

All comparison operators work: `=`, `<>`, `<`, `>`, `<=`, `>=`.

## Updating Data

### Set a Property

```cypher
MATCH (n:Person {name: 'Alice'}) SET n.age = 31 RETURN n
```

Finds Alice and updates her age property.

### Set Multiple Properties

```cypher
MATCH (n:Person {name: 'Alice'}) SET n.age = 31, n.city = 'NYC' RETURN n
```

Multiple comma-separated SET assignments.

## Deleting Data

### Delete a Node

```cypher
MATCH (n:Person {name: 'Alice'}) DELETE n
```

Deletes the node. Note: this does not automatically remove connected relationships (use `DETACH DELETE` for that, though edge removal is a TODO in the current implementation).

### Detach Delete

```cypher
MATCH (n:Person {name: 'Alice'}) DETACH DELETE n
```

Deletes the node and all its relationships. **Note**: The `DETACH` flag is parsed and passed through, but the executor does not yet remove edges before deleting the node. See [Implementation Roadmap](13-implementation-roadmap.md).

## Ordering and Pagination

### Order Results

```cypher
MATCH (n:Person) RETURN n.name ORDER BY n.name DESC
```

Orders results by `n.name` in descending order. Omit `DESC` for ascending (the default).

### Limit Results

```cypher
MATCH (n:Person) RETURN n.name LIMIT 10
```

Returns at most 10 rows.

### Paginate with Skip

```cypher
MATCH (n:Person) RETURN n.name ORDER BY n.name SKIP 20 LIMIT 10
```

Skips the first 20 rows, then returns the next 10 — page 3 of 10-per-page results.

## Query Chaining

### WITH Clause

```cypher
MATCH (n:Person) WITH n.name AS name RETURN name
```

Pipes the result of `MATCH` through `WITH`, renaming `n.name` to `name`.

### WITH with Filter

```cypher
MATCH (n:Person) WITH n WHERE n.age > 25 RETURN n.name
```

Filters within the `WITH` clause using a trailing `WHERE`.

## Unwinding Lists

### Expand a List into Rows

```cypher
UNWIND [1, 2, 3] AS x RETURN x
```

Produces three rows, one per list element.

### Unwind with Parameters

```rust
engine.execute(
    "UNWIND $ids AS id MATCH (n) WHERE id(n) = id RETURN n.name",
    &[("ids", Value::Array(vec![Value::Integer(1), Value::Integer(2)]))],
).await?;
```

**Note**: The `id()` function is not yet implemented in the executor. This example shows the intended pattern.

## Vector Search

### Basic Vector Search

```cypher
CALL db.vector.search('Embedding', $query_vec, 10)
YIELD node, score
RETURN node, score
```

Searches the HNSW index for the 10 nearest neighbors of `$query_vec` among `:Embedding` nodes.

### Vector Search with Threshold

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
RETURN node, score
```

Only returns results with a similarity score >= 0.7.

## Hybrid Vector + Graph Queries

### Find Similar, Then Traverse to Content

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS]->(doc:Document)
RETURN doc.title AS title, doc.body AS body, score
ORDER BY score DESC
```

The RAG pattern: vector search finds similar embedding nodes, then graph traversal follows `:EMBEDS` relationships to the content documents. A single query does both.

### Vector Search with Additional Filtering

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS]->(doc:Document)
WHERE doc.category = 'technical'
RETURN doc.title AS title, score
ORDER BY score DESC LIMIT 5
```

Combines vector search, graph traversal, and property filtering in one query.

### Similarity in WHERE

```cypher
MATCH (n:Embedding)
WHERE db.vector.similarity(n.vector, $query) > 0.8
RETURN n
```

Filters embedding nodes by similarity to a query vector. **Note**: This currently performs a scan over all `:Embedding` nodes and computes similarity for each. The optimizer does not yet route this to the HNSW index automatically. For production use, prefer the `CALL db.vector.search` procedure which uses the index directly.

## Parameterized Queries

### Using Parameters

```cypher
MATCH (n:Person) WHERE n.name = $name AND n.age > $minAge RETURN n
```

```rust
engine.execute(
    "MATCH (n:Person) WHERE n.name = $name AND n.age > $minAge RETURN n",
    &[("name", "Alice".into()), ("minAge", 25i64.into())],
).await?;
```

Parameters prevent injection and allow the query to be reused with different values.

## Current Limitations

The following common Cypher patterns are **not yet fully implemented**:

| Pattern | Status |
|---|---|
| Relationship creation in `CREATE` | Parsed, nodes created, relationships not wired |
| `MERGE` (upsert) | Parsed, not executed |
| Variable-length paths (`*1..3`) | Parsed, executor returns empty |
| Aggregation (`count`, `sum`, `avg`) | Not implemented |
| `UNION` | Parsed, not executed |
| `OPTIONAL MATCH` null fills | Parsed, not executed |
| `DETACH DELETE` edge removal | Flag passed, edges not removed |
| `id()` function | Not implemented |
| String functions (`toUpper`, `trim`) | Not implemented |
| List functions (`size`, `range`) | Not implemented |
| Vector index detection in WHERE | Not optimized (use CALL procedure instead) |

See [Implementation Roadmap](13-implementation-roadmap.md) for the full status and plan.

## What's Next

- **[11 — RAG Patterns](11-rag-patterns.md)** — Deeper RAG-specific patterns.
- **[09 — API Reference](09-api-reference.md)** — Full API documentation.
- **[06 — Cypher Query Language](06-cypher-query-language.md)** — Syntax and grammar details.
