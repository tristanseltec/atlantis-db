# 06 — Cypher Query Language

**Crate:** `atlantis-cypher`

This chapter documents the Cypher-compatible query language: the lexer, parser, AST structure, supported clauses, expression grammar, and vector extension functions.

## Supported Cypher Subset

Atlantis DB implements a practical subset of Cypher, the query language used by Neo4j and FalkorDB. The supported syntax is syntactically valid Cypher, though not all Cypher features are implemented in v1.

### Supported Clauses

| Clause | Status | Description |
|---|---|---|
| `MATCH` | Implemented | Pattern matching with node and relationship patterns |
| `OPTIONAL MATCH` | Parsed | Optional match (return nulls for non-matches) |
| `WHERE` | Implemented | Filtering with comparison, logical, and string operators |
| `RETURN` | Implemented | Projection with aliases, DISTINCT, ORDER BY, SKIP, LIMIT |
| `CREATE` | Implemented | Create nodes (relationship creation is a TODO) |
| `MERGE` | Parsed | Upsert (execution is a TODO) |
| `DELETE` | Implemented | Delete nodes |
| `DETACH DELETE` | Parsed | Delete node with relationships (edge removal is a TODO) |
| `SET` | Implemented | Set properties |
| `REMOVE` | Parsed | Remove properties/labels |
| `WITH` | Implemented | Query chaining and sub-query piping |
| `UNWIND` | Implemented | Expand a list into rows |
| `CALL ... YIELD` | Implemented | Vector search procedure call |
| `ORDER BY` | Implemented | Result ordering (ASC/DESC) |
| `LIMIT` / `SKIP` | Implemented | Pagination |
| `DISTINCT` | Parsed | Deduplication |

### Limitations in v1

- Relationship creation in `CREATE` is not yet wired through to the executor.
- `MERGE` is parsed but not executed.
- Variable-length path patterns (`*1..3`) are parsed but the executor returns empty results.
- Aggregation functions (`count`, `sum`, `avg`) are not yet implemented.
- `UNION` is parsed but not executed.
- `OPTIONAL MATCH` is parsed but does not produce null fills.

See [Implementation Roadmap](13-implementation-roadmap.md) for the full list of pending items.

## Lexer

**Files:** [`crates/atlantis-cypher/src/lexer/token.rs`](../crates/atlantis-cypher/src/lexer/token.rs), [`crates/atlantis-cypher/src/lexer/tokenizer.rs`](../crates/atlantis-cypher/src/lexer/tokenizer.rs)

The tokenizer converts a query string into a stream of `SpannedToken`s (token + byte span):

```rust
pub struct SpannedToken {
    pub token: Token,
    pub span: std::ops::Range<usize>,
}
```

### Token Types

Tokens cover keywords, punctuation, literals, and identifiers:

```
Keywords:  MATCH OPTIONAL WHERE RETURN CREATE MERGE DELETE DETACH SET REMOVE
          WITH ORDER BY LIMIT SKIP AND OR NOT XOR CALL YIELD DISTINCT AS
          ON UNION ALL UNWIND IN IS NULL TRUE FALSE STARTS ENDS CONTAINS

Punct:    ( ) { } [ ] : , . | = <> < > <= >= + - * / % --> <-- <-->

Literals: Integer(i64) Float(f64) String(String) Identifier(String)
Special:  Parameter(String) Eof
```

### Tokenizer Features

- **Case-insensitive keywords**: `match`, `MATCH`, `Match` all produce `Token::Match`. Identifiers retain their original case.
- **String literals**: Single (`'...'`) and double (`"..."`) quoted, with escape sequences (`\n`, `\t`, `\r`, `\\`, `\'`, `\"`).
- **Numbers**: Integers (`42`, `-5`) and floats (`3.14`). Negative numbers are tokenized as part of the number literal.
- **Parameters**: `$name` produces `Token::Parameter("name")`.
- **Comments**: Line comments (`// ...`) and block comments (`/* ... */`) are skipped.
- **Arrows**: `-->` (ArrowRight), `<--` (ArrowLeft), `<-->` (ArrowBoth), `-[...]->` syntax.

### Error Handling

The tokenizer returns `AtlantisError::Parse(String)` for:
- Unterminated string literals
- Unexpected characters (e.g., `!` without `=`)
- Invalid number formats

## Parser

**File:** [`crates/atlantis-cypher/src/parser/statement.rs`](../crates/atlantis-cypher/src/parser/statement.rs)

The parser is a hand-written recursive-descent parser that consumes the token stream and produces an AST. Despite `chumsky` being listed as a workspace dependency, the current implementation is hand-written for full control over error messages and Cypher's syntax quirks.

### Parser Structure

```rust
struct Parser<'a> {
    tokens: &'a [SpannedToken],
    pos: usize,
}
```

The parser maintains a position cursor and provides methods like `current()`, `advance()`, and `expect(token)`.

### Operator Precedence

Expression parsing follows standard precedence (lowest to highest):

```
OR          (lowest)
AND
Comparison   (= <> < > <= >=)
Additive     (+ -)
Multiplicative (* / %)
Unary        (- NOT)
Primary      (literals, variables, property access, function calls, parens)
```

## AST

**File:** [`crates/atlantis-cypher/src/parser/ast.rs`](../crates/atlantis-cypher/src/parser/ast.rs)

The AST is a tree of types representing the parsed query:

```
Statement
  └── Query
       ├── clauses: Vec<Clause>
       │    ├── Match(MatchClause { pattern, optional })
       │    ├── Where(Expression)
       │    ├── Create(CreateClause { pattern })
       │    ├── Merge(MergeClause { pattern })
       │    ├── Delete(DeleteClause { detach, expressions })
       │    ├── Set(SetClause { assignments })
       │    ├── Remove(RemoveClause { items })
       │    ├── With(WithClause { distinct, items, order_by, skip, limit, where })
       │    ├── Unwind(UnwindClause { expression, variable })
       │    └── Call(CallClause { procedure, arguments, yield_items })
       │
       └── result: Option<ReturnClause>
            ├── distinct: bool
            ├── items: Vec<ReturnItem { expression, alias }>
            ├── order_by: Option<Vec<OrderItem { expression, descending }>>
            ├── skip: Option<Expression>
            └── limit: Option<Expression>
```

### Pattern Structure

```
Pattern
  └── elements: Vec<PatternElement>
       ├── Node(NodePattern { variable, labels, properties })
       └── Relationship(RelationshipPattern { variable, rel_type, direction, properties, length })
```

A pattern is an interleaved sequence of node and relationship patterns. For example, `(a:Person)-[:KNOWS]->(b:Person)` parses as:

```
Node(a:Person) → Relationship(KNOWS, Outgoing) → Node(b:Person)
```

### Expression Types

```rust
pub enum Expression {
    Variable(String),              // n
    Property(Box<Expression>, String),  // n.name
    Literal(Value),               // 'Alice', 42, true
    Parameter(String),            // $name
    FunctionCall(String, Vec<Expression>),  // db.vector.similarity(a, b)
    BinaryOp { left, op, right },  // a.name = 'Alice'
    UnaryOp { op, operand },       // NOT x, -5
    List(Vec<Expression>),         // [1, 2, 3]
    Map(Vec<(String, Expression)>),  // {key: value}
    Case { cases, default },       // CASE WHEN ... THEN ... ELSE ... END
}
```

### Binary Operators

| Operator | Token(s) | Description |
|---|---|---|
| `Eq` | `=` | Equality |
| `NotEq` | `<>` | Inequality |
| `Lt`, `Gt` | `<`, `>` | Comparison |
| `LtEq`, `GtEq` | `<=`, `>=` | Comparison |
| `Add`, `Sub` | `+`, `-` | Arithmetic |
| `Mul`, `Div`, `Mod` | `*`, `/`, `%` | Arithmetic |
| `And`, `Or`, `Xor` | `AND`, `OR`, `XOR` | Logical |
| `In` | `IN` | Membership |
| `StartsWith`, `EndsWith`, `Contains` | `STARTS WITH`, `ENDS WITH`, `CONTAINS` | String ops |
| `IsNull`, `IsNotNull` | `IS NULL`, `IS NOT NULL` | Null checks |

## Vector Extension Functions

Atlantis DB extends Cypher with vector similarity functions:

### `db.vector.search`

A procedure called via `CALL`:

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
RETURN node, score
```

| Argument | Type | Description |
|---|---|---|
| Label | String literal | Node label to search (e.g., `'Embedding'`) |
| Query vector | Expression (evaluates to `Vector`) | The query embedding |
| k | Integer literal | Number of nearest neighbors |
| Score threshold | Float literal (optional) | Minimum similarity (0.0–1.0) |

Yields: `node` (the matching `NodeId`) and `score` (cosine similarity).

### `db.vector.similarity`

A function usable in `WHERE` clauses:

```cypher
MATCH (n:Embedding)
WHERE db.vector.similarity(n.vector, $query) > 0.8
RETURN n
```

Computes cosine similarity between two vectors. Returns a float (0.0–1.0).

## Query Examples

### Basic Pattern Match

```cypher
MATCH (n:Person) RETURN n
```

### Filtered Traversal

```cypher
MATCH (a:Person)-[:KNOWS]->(b:Person)
WHERE a.name = 'Alice' AND b.age > 25
RETURN b.name AS friend
```

### Create with Properties

```cypher
CREATE (n:Person {name: 'Alice', age: 30}) RETURN n
```

### Parameterized Query

```cypher
MATCH (n:Person) WHERE n.name = $name RETURN n.age
```

### Order and Paginate

```cypher
MATCH (n:Person) RETURN n.name ORDER BY n.name DESC LIMIT 10 SKIP 5
```

### Set Property

```cypher
MATCH (n:Person {name: 'Alice'}) SET n.age = 31 RETURN n
```

### Delete

```cypher
MATCH (n:Person {name: 'Alice'}) DELETE n
```

### With Clause Chaining

```cypher
MATCH (n:Person) WITH n.name AS name RETURN name
```

### Vector Search + Traversal

```cypher
CALL db.vector.search('Embedding', $query_vec, 10, 0.7)
YIELD node, score
MATCH (node)-[:EMBEDS]->(doc:Document)
RETURN doc.title AS title, score
ORDER BY score DESC
```

## Design Decisions

**Why a hand-written recursive-descent parser?**
Full control over error messages, Cypher's specific syntax quirks (arrows, inline properties, variable-length paths), and the ability to add vector extension functions cleanly. Parser combinator libraries (chumsky, pest) are powerful but make it harder to produce helpful error messages for complex grammars like Cypher. The chumsky dependency is retained in the workspace for potential future use.

**Why case-insensitive keywords?**
Cypher is conventionally case-insensitive for keywords (Neo4j allows `match`, `MATCH`, `Match`). Identifiers and labels retain their original case. This follows the Cypher convention and reduces friction for users coming from Neo4j.

**Why a `CALL ... YIELD` syntax for vector search?**
This is the Cypher standard for procedure calls. It fits naturally into the query pipeline — `YIELD` produces variables that can be used in subsequent `MATCH` and `RETURN` clauses. This is the same pattern FalkorDB uses for vector search.

## What's Next

- **[07 — Query Planner & Executor](07-query-planner-executor.md)** — How the AST becomes a logical plan and gets executed.
- **[10 — Query Cookbook](10-query-cookbook.md)** — More query examples and patterns.
- **[11 — RAG Patterns](11-rag-patterns.md)** — RAG-specific query patterns.
