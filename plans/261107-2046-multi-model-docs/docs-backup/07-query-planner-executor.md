# 07 — Query Planner & Executor

**Crate:** `atlantis-cypher` (planner), `atlantis-engine` (executor)

This chapter documents how a parsed Cypher query becomes a logical plan, how that plan is optimized, and how the executor walks it to produce result rows.

## Pipeline Overview

```
AST (Statement)
  │
  ▼
planner::plan()     →  LogicalOperator tree
  │
  ▼
planner::optimize() →  LogicalOperator tree (transformed)
  │
  ▼
executor::execute_plan()  →  (columns, Vec<Row>)
```

## Logical Plan Operators

**File:** [`crates/atlantis-cypher/src/planner/logical.rs`](../crates/atlantis-cypher/src/planner/logical.rs)

The logical plan is a tree of `LogicalOperator` variants:

| Operator | Description |
|---|---|
| `Empty` | No operations (empty query) |
| `Sequence(Vec<LogicalOperator>)` | Execute operators in order |
| `NodeByLabelScan { var, label }` | Scan all nodes with a given label |
| `AllNodesScan { var }` | Scan all nodes |
| `Expand { from, to, rel_type, direction }` | Traverse from one node to neighbors via a relationship type |
| `VarLenExpand { from, to, min, max }` | Variable-length (multi-hop) traversal — TODO |
| `VectorSearch { label, query, k, threshold }` | HNSW similarity search |
| `Filter { predicate }` | Filter rows by a boolean expression |
| `Project { items }` | Select expressions with optional aliases |
| `Sort { keys }` | Sort by expressions (ASC/DESC) |
| `Limit { count }` | Limit to N rows |
| `Skip { count }` | Skip N rows |
| `CreateNode { var, labels, properties }` | Create a new node |
| `CreateRelationship { from, to, rel_type }` | Create a relationship — TODO |
| `SetProperty { target, key, value }` | Set a property on a node |
| `Delete { targets, detach }` | Delete nodes |
| `Unwind { expression, variable }` | Expand a list into individual rows |

Each operator (except scans and vector search) takes an `input: Box<LogicalOperator>` — the operator whose output it consumes. This forms a pipeline where data flows from scans upward through filters, projections, and sorts.

## Planning: AST → Logical Plan

**File:** [`crates/atlantis-cypher/src/planner/mod.rs`](../crates/atlantis-cypher/src/planner/mod.rs)

The planner walks the AST's clause list and builds the logical plan incrementally:

```rust
pub fn plan(statement: &Statement) -> Result<LogicalOperator, AtlantisError>
```

### Planning Logic

For each clause in the query, the planner transforms the current plan:

1. **MATCH** (`plan_match`): For each pattern element:
   - **Node pattern**: If it has labels, emit `NodeByLabelScan` (using the first label). If no labels, emit `AllNodesScan`. The node variable is bound in the row.
   - **Relationship pattern**: Emit `Expand` from the last-scanned node variable to a new variable, using the relationship type and direction.

2. **WHERE**: Wrap the current plan in `Filter { input, predicate }`.

3. **CREATE** (`plan_create`): Emit `CreateNode` operators for each node pattern in a `Sequence`. (Relationship creation is a TODO.)

4. **SET**: Emit `SetProperty` operators in a `Sequence`.

5. **DELETE**: Emit `Delete` wrapping the current plan.

6. **CALL** (`plan_call`): If the procedure is `db.vector.search`, emit `VectorSearch` with the parsed arguments (label, query expression, k, threshold). The `YIELD` items determine the output variable names.

7. **WITH**: Emit `Project` for the WITH items, then optionally `Filter` if a `WHERE` follows the WITH.

8. **UNWIND**: Emit `Unwind` wrapping the current plan.

9. **RETURN** (final clause): Emit `Project` for the return items, then `Sort` (ORDER BY), `Skip`, and `Limit` as specified.

### Example: Planning a MATCH + WHERE + RETURN

Query:
```cypher
MATCH (a:Person)-[:KNOWS]->(b:Person) WHERE a.name = 'Alice' RETURN b.name AS friend
```

Logical plan:
```
Project [b.name AS friend]
  └── Filter [a.name = 'Alice']
       └── Expand { from: "a", to: "b", rel_type: KNOWS, direction: Outgoing }
            └── NodeByLabelScan { var: "a", label: Person }
```

### Example: Planning a Vector Search

Query:
```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS]->(doc:Document)
RETURN doc.title AS title, score
```

Logical plan:
```
Project [doc.title AS title, score]
  └── Expand { from: "node", to: "doc", rel_type: EMBEDS, direction: Outgoing }
       └── VectorSearch { label: Embedding, query: $query_vec, k: 10, threshold: 0.7,
                          node_var: "node", score_var: "score" }
```

## Optimizer

**File:** [`crates/atlantis-cypher/src/planner/optimizer.rs`](../crates/atlantis-cypher/src/planner/optimizer.rs)

The optimizer applies transformation rules to the logical plan. In the current implementation, it is a **pass-through** — the plan is returned unchanged:

```rust
pub fn optimize(plan: LogicalOperator) -> LogicalOperator {
    plan
}
```

Planned optimizations (see [Implementation Roadmap](13-implementation-roadmap.md)):
- **Filter pushdown**: Move `Filter` operators closer to scans to reduce intermediate row counts.
- **Limit pushdown**: Push `Limit` into scans where possible.
- **Vector index detection**: Recognize vector similarity predicates in `WHERE` and route them to the HNSW index instead of scanning.
- **Dead code elimination**: Remove unused projections.

## Executor

**File:** [`crates/atlantis-engine/src/executor.rs`](../crates/atlantis-engine/src/executor.rs)

The executor walks the logical plan tree recursively, producing rows of data. Each row is a `HashMap<String, Value>` mapping variable names to values.

```rust
pub struct QueryExecutor<'a> {
    store: &'a mut GraphStore,
    vector_index: Option<&'a HnswIndex>,
}
```

### Execution Model

The executor's `execute_plan` method matches on each `LogicalOperator` variant:

| Operator | Execution |
|---|---|
| `Empty` | Returns empty columns and rows |
| `NodeByLabelScan` | Calls `store.get_nodes_by_label(label)`, emits one row per node ID |
| `AllNodesScan` | Calls `store.get_all_nodes()`, emits one row per node ID |
| `Expand` | For each input row, gets the source node's neighbors via `store.get_neighbors(from, rel_type)`, emits a row per neighbor |
| `Filter` | Evaluates the predicate expression for each row, keeps rows where it's `true` |
| `Project` | Evaluates each expression, binds results to column names (from alias or inferred) |
| `Sort` | Evaluates sort key expressions, sorts rows (ascending/descending) |
| `Limit` | Takes the first N rows |
| `Skip` | Drops the first N rows |
| `CreateNode` | Evaluates property expressions, calls `store.insert_node(labels, properties)`, binds the new ID |
| `SetProperty` | For each row, evaluates the target and value, calls `store.set_property` |
| `Delete` | For each row, evaluates target expressions to get node IDs, calls `store.delete_node` |
| `VectorSearch` | Evaluates the query vector, calls `index.search` or `index.search_with_params`, emits rows with node ID and score |
| `Unwind` | For each input row, evaluates the list expression, emits one row per list element |
| `Sequence` | Executes each sub-operator in order, returning the last result |

### Node Representation in Rows

Nodes are represented in rows as `Value::Integer(node_id.0 as i64)`. Property access (`n.name`) works by evaluating the variable to get the node ID, then calling `store.get_node_hot(id)` and reading the property.

### Expression Evaluation

The `eval_expr` method evaluates expressions in the context of a row and parameter map:

| Expression | Evaluation |
|---|---|
| `Literal(v)` | Returns `v` |
| `Variable(name)` | Looks up in row, falls back to params |
| `Property(inner, prop)` | Evaluates `inner` to get a node ID, fetches the node, returns the property |
| `Parameter(name)` | Looks up in params |
| `BinaryOp { left, op, right }` | Evaluates both sides, applies the operator |
| `UnaryOp { op, operand }` | Evaluates the operand, applies negation or NOT |
| `FunctionCall(name, args)` | Dispatches to built-in functions (currently `db.vector.similarity`) |
| `List(items)` | Evaluates each item, returns as `Array` |

#### Binary Operator Semantics

| Operator | Behavior |
|---|---|
| `Eq` / `NotEq` | Uses `Value::PartialEq` (cross-type numeric comparison) |
| `Lt`, `Gt`, `LtEq`, `GtEq` | Uses `Value::PartialOrd` |
| `And` / `Or` | Both operands must be `Bool`; returns `Bool` |
| `Add` | Integer+Integer → Integer, Float+Float → Float, String+String → concatenation |
| `Sub`, `Mul`, `Div` | Numeric only |
| `In` | Checks if left value is in the right `Array` |

### Vector Search Execution

When the executor encounters `VectorSearch`:

1. Get the HNSW index (error if none available).
2. Evaluate the query expression to get a `Value::Vector`.
3. If a threshold is set, call `search_with_params`; otherwise call `search`.
4. For each result `(NodeId, distance)`, create a row binding `node_var` to the node ID and `score_var` to `1.0 - distance` (converting distance to similarity).
5. Label filtering is not yet applied in the executor — the vector search returns all results regardless of the label argument. This is a known limitation.

### What's Not Implemented in the Executor

- `VarLenExpand`: Returns empty results (TODO).
- `CreateRelationship`: Returns empty results (TODO).
- `DETACH DELETE`: Edge removal before node deletion is not wired (TODO).
- Aggregation functions: Not implemented.
- `MERGE` execution: Not implemented (parsed but not planned).

See [Implementation Roadmap](13-implementation-roadmap.md) for details.

## Design Decisions

**Why a volcano-style iterator model?**
The executor uses a pull-based (Volcano) model where each operator calls its child to get input rows, processes them, and returns output rows. This is simple to implement and reason about, and works well for the current scale. A push-based (vectorized) model would be faster for large datasets but adds complexity.

**Why store node IDs in rows instead of full node objects?**
Node IDs are `u64` (8 bytes), cheap to copy and pass around. Storing full `Node` objects in rows would waste memory and require cloning property maps. The executor fetches nodes from the store on demand when property access is needed. This is efficient because the hot tier's `Vec<Option<Node>>` gives O(1) access by ID.

**Why is the optimizer a pass-through?**
For v1, the planner already produces reasonable plans. The most impactful optimization (vector index detection in WHERE clauses) requires more sophisticated analysis. Filter pushdown and other rules are planned but not blocking for v1 functionality.

## What's Next

- **[08 — Engine Runtime](08-engine-runtime.md)** — How the executor is invoked from the actor.
- **[10 — Query Cookbook](10-query-cookbook.md)** — Practical query examples.
- **[13 — Implementation Roadmap](13-implementation-roadmap.md)** — Pending executor and optimizer work.
