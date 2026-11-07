# 03 — Data Model

> **Status:** Partially implemented — the record model types (`RecordId`, `Record`, `Edge`, extended `Value` except `Decimal` and `Range`) and namespaces, databases and tables are implemented in the Rust record API; Cypher still uses the property graph types; schema, indexes, record links and Cypher access to records are Planned — Phase 2
> **Crates:** `atlantis-core`, `atlantis-storage`, `atlantis-kv`, `atlantis-index` (planned)

Atlantis DB stores every kind of data (graph nodes, edges, documents, relational rows,
time-series points) as **records** in **tables**. This chapter defines that unified
record model, the `RecordId` that identifies every record, and the `Value` type system
shared by all crates. The last part documents the property graph types that Cypher
still uses and how they map onto the record model.

The record model is implemented in `atlantis-core` and stored by the record store
([Storage Engine](04-storage-engine.md)), and it is reachable from Rust through record
transactions ([API Reference](21-api-reference.md#record-transactions)). It is **not yet
reachable from Cypher**: `Engine::execute` still runs on the property graph store, and
the two stores do not share data. Cypher examples in this chapter are therefore Phase 2
unless marked otherwise.

## Record Model (Partially implemented — Phase 2)

### Hierarchy: Namespace → Database → Table → Record (Implemented in storage; DDL Phase 2)

```
Namespace  acme                       (tenant boundary: users, roles, access)
└── Database  shop                    (unit of USE, transactions, schema)
    ├── Table  Person                 (node table, SCHEMAFULL)
    ├── Table  Order                  (document/relational table)
    ├── Table  WORKS_AT  TYPE EDGE    (edge table)
    └── Table  Reading   TYPE TIMESERIES
        └── Record  Reading:['sensor-1', d'2026-11-07T00:00:00Z']
```

- A **namespace** is the tenancy boundary. Users, roles and access methods can be
  defined on a namespace (see [Security & Auth](18-security-auth.md)).
- A **database** belongs to one namespace. It is the unit a session selects with `USE`,
  the scope of schema definitions and the scope of a transaction.
- A **table** belongs to one database and holds records of one kind: a normal table
  (schemaless by default, or schemafull), an **edge table** (`TYPE EDGE`), or a
  **time-series table** (`TYPE TIMESERIES`, Phase 3).
- A **record** lives in exactly one table.

Embedded engines start in namespace `atlantis`, database `default`, so single-tenant
applications never need to mention either. In Rust, `DatabaseRef` names a database and
`Engine::use_db(ns, db)` selects one for record transactions. Namespace, database,
table, label and field names must be ASCII identifiers (letters, digits, `_`, not
starting with a digit, at most 255 bytes). User table names must not start with `_`,
which is reserved for the system.

Today tables are created implicitly (schemaless) on first write, or with
`RecordTx::define_table`. The Cypher statements below are Phase 2:

```cypher
CREATE NAMESPACE acme;
CREATE DATABASE acme.shop;
USE acme.shop;
```

The full grammar for namespaces, databases, tables and fields is owned by
[Schema & DDL](07-schema-ddl.md). How the hierarchy maps to storage keys is owned by
[Storage Engine](04-storage-engine.md).

### Records and RecordId

**Implemented** in [`crates/atlantis-core/src/record.rs`](../crates/atlantis-core/src/record.rs),
except the Cypher forms (`id()`, `rid()`, `CREATE` with `id`), which are Phase 2.

Every record has a **RecordId** made of the table name and a key. The RecordId is
stored in the record's `id` field and is itself a `Value`, so it can be stored in other
records, compared, indexed and returned from queries.

| Key kind | Example (text form) | Typical use |
|---|---|---|
| ULID (default) | `Person:01JC3Q8Z6V4N0Y2R7T5KX9WAHD` | Auto-generated when no key is given; time-ordered |
| Integer | `Person:42` | Imported relational data, counters |
| String | `Person:alice` | Natural keys, slugs |
| UUID | `Session:u'0193…'` | Keys issued by other systems |
| Array (composite, ordered) | `Reading:['sensor-1', d'2026-11-07T00:00:00Z']` | Time series, range scans by key prefix |

Text form rules, as implemented by `RecordId`'s `Display` and `FromStr`:

- `table:key`. A key of digits (optionally signed) parses as an integer.
- A string key that is not a plain identifier is wrapped in backticks:
  `` Person:`hello world` ``. A doubled backtick escapes a backtick.
- A UUID key is written `u'…'`.
- A 26-character key that is a valid ULID parses as a ULID; any other key is a string.
- Array keys can be displayed but not parsed from text. The `d'…'` datetime literal in
  the table is the planned Cypher syntax.

Rules:

- Setting `id` at creation chooses the key: either a plain key value
  (`{id: 'alice'}`) or a full RecordId of the same table (`{id: rid('Person', 'alice')}`
  in Cypher, Phase 2). Integers, non-empty strings, UUIDs and non-empty arrays can be
  keys. Without `id`, a new ULID is generated. ULIDs generated by one process are
  strictly increasing. This works today through `RecordTx::create` and `create_node`.
- Keys are ordered within a table by the byte-ordered key encoding, so integer keys sort
  numerically and array keys sort element by element. That ordering is what makes key
  range scans and time-series scans cheap (see
  [Key-Value & Time-Series](13-key-value-time-series.md)).
- A record's table and key never change. Moving data to another table means creating a
  new record.
- **Known limitation:** `Integer(1)` and `Float(1.0)` compare equal as values but encode
  to different keys, so `Reading:[1]` and `Reading:[1.0]` are two different records.

The text form (`Person:alice`) is for display, logs and documentation. Inside a query,
pass a RecordId as a parameter or build one with `rid()`:

```cypher
// Build a RecordId and look up one record by it
MATCH (p:Person) WHERE id(p) = rid('Person', 'alice') RETURN p;

// Choose the key at creation time instead of an auto-generated ULID
CREATE (p:Person {id: 'alice', name: 'Alice'});                 // key 'alice'
CREATE (p:Person {id: rid('Person', 'bob'), name: 'Bob'});      // same, as a RecordId

// Read the RecordId of any node or relationship
MATCH (p:Person {name: 'Alice'}) RETURN id(p);
```

### Graph Mapping (Implemented in the record store; Cypher Phase 1–2)

The property graph is a view over records. `RecordTx::create_node` and `relate`
implement the rules below today; Cypher `CREATE` does not use them yet.

- **A node is a record.** The node's **first label is its table**. Further labels are
  **secondary labels**: an indexed tag set stored on the record, so
  `MATCH (n:Admin)` stays a cheap lookup even though `Admin` is not a table. A node
  created without any label goes into the reserved schemaless table `_default`; table
  names starting with `_` are reserved for the system.
- **A relationship is a record in an edge table** named after the relationship type.
  Every edge record has mandatory `in` and `out` fields holding RecordIds. `in` is the
  start node and `out` is the end node; Cypher exposes the same values as
  `startNode(r)` and `endNode(r)` (Phase 2). In Rust, `Edge` holds them as `from` and
  `to`, serialized as `in` and `out`. An edge cannot be an endpoint of another edge.
- Edge tables are ordinary tables. Restricting them to particular endpoint tables works
  today (`TableKind::Edge { from, to }`). Schemafull edge tables, typed fields, indexes
  and constraints are Phase 2.

```cypher
CREATE TABLE Person SCHEMAFULL;
CREATE TABLE Company SCHEMALESS;
CREATE TABLE WORKS_AT TYPE EDGE FROM Person TO Company;

MATCH (p:Person {name: 'Alice'}), (c:Company {name: 'Atlantis'})
CREATE (p)-[:WORKS_AT {since: 2024}]->(c);
```

That statement produces one edge record:

| Field | Value |
|---|---|
| `id` | `WORKS_AT:01JC3R...` (auto ULID) |
| `in` | `Person:alice` |
| `out` | `Company:atlantis` |
| `since` | `2024` |

The storage layer also writes adjacency entries for both directions, so traversal from
either end does not scan the edge table. The in-memory CSR adjacency cache is rebuilt
from those entries. See [Storage Engine](04-storage-engine.md) and
[Graph Model](09-graph-model.md).

### Document Mapping (Value type implemented; Cypher access Phase 2)

A record is a document: a map whose values may be nested maps (`Object`) and arrays.
Tables are **schemaless by default**, so any shape is accepted. Declaring a table
`SCHEMAFULL` makes the declared fields the only accepted fields.

```cypher
CREATE (o:Order {
  status: 'open',
  address: {street: '12 Le Loi', city: 'Hue', geo: point({longitude: 107.59, latitude: 16.46})},
  lines: [{sku: 'A-1', qty: 2}, {sku: 'B-7', qty: 1}]
});

MATCH (o:Order) WHERE o.address.city = 'Hue' RETURN o.lines[0].sku;
MATCH (o:Order) SET o.address.city = 'Da Nang';
MATCH (o:Order) SET o += {priority: 'high'};
```

In Rust, `Record.data` is an `Object` (`BTreeMap<String, Value>`), nested values use
`Value::Object`, and `Value::get_path("address.city")` follows nested objects.
`RecordTx::merge` sets top-level fields and removes fields set to `Null`. Nested paths,
list operations and merge semantics in Cypher are covered in
[Document & Relational](08-document-relational.md).

### Relational Mapping (Planned — Phase 2)

Relational modelling uses the same records:

- **Tables with typed fields** (`SCHEMAFULL` plus `CREATE FIELD ... TYPE ...`).
- **Record links**: a field of type `record<Customer>` holds a RecordId pointing into
  another table. Property access through a record link dereferences it automatically,
  so `o.customer.name` reads the linked customer's `name` without a join clause or a
  separate fetch step.
- **Secondary indexes and constraints** on fields (`atlantis-index`).
- **Joins** expressed as Cypher patterns or record-link access, planned by the
  cost-based optimizer (see [Query Planner & Executor](15-query-planner-executor.md)).

```cypher
CREATE TABLE Order SCHEMAFULL;
CREATE FIELD customer ON Order TYPE record<Customer>;
CREATE FIELD total ON Order TYPE decimal ASSERT value >= 0;

MATCH (o:Order) WHERE o.customer.country = 'VN'
RETURN o.customer.name AS customer, sum(o.total) AS revenue;
```

A record link is a value, not a relationship. Use an edge table when the connection
needs its own fields, must be traversed in both directions, or takes part in
variable-length paths. Use a record link for a plain foreign-key style reference.

### Other Models on the Same Records

| Model | How it maps | Chapter | Status |
|---|---|---|---|
| Vector | A `vector<f32, N>` field plus a vector index on it | [Vector Index](10-vector-index.md) | Partial |
| Full-text | A full-text index over one or more string fields | [Full-Text Search](11-full-text-search.md) | Phase 3 |
| Geospatial | A `point` or `geometry` field plus a point index | [Geospatial](12-geospatial.md) | Phase 3 |
| Time series | A `TYPE TIMESERIES` table whose array key ends with a datetime | [Key-Value & Time-Series](13-key-value-time-series.md) | Phase 3 |
| Key-value | `db.kv.*` entries in the database's key space, outside tables | [Key-Value & Time-Series](13-key-value-time-series.md) | Phase 3 |
| Change feeds, events, live queries | Observers of record changes in a table | [Live Queries & Events](14-live-queries-events.md) | Phase 4 |

## Value Type System (Partially implemented)

`Value` remains the single value type used by storage, indexes, the query language and
every API. The target set extends the original seven variants. "Rust" in the Status
column means the variant exists and can be stored through record transactions, but
Cypher cannot produce it yet:

| Variant | Payload | DDL type name | How to produce it in Cypher | Status |
|---|---|---|---|---|
| `Null` | — | (allowed by `option<T>`) | `null` | Implemented |
| `Bool` | `bool` | `bool` | `true`, `false` | Implemented |
| `Integer` | `i64` | `int` | `42` | Implemented |
| `Float` | `f64` | `float` | `3.14` | Implemented |
| `Decimal` | 128-bit decimal | `decimal` | Parameter, or a value written to a `decimal` field | Phase 2 |
| `String` | UTF-8 | `string` | `'text'` | Implemented |
| `Bytes` | byte array | `bytes` | Parameter | Rust; Cypher Phase 2 |
| `Datetime` | UTC, nanoseconds since the epoch in an `i64` (about 1678–2262); RFC 3339 text | `datetime` | `d'2026-11-07T00:00:00Z'`, `time.now()` | Rust; Cypher Phase 2 |
| `Duration` | signed span in `i64` nanoseconds; text `1h30m` or ISO 8601 `PT5M` (no years or months) | `duration` | `duration('PT5M')` | Rust; Cypher Phase 2 |
| `Uuid` | 128-bit UUID | `uuid` | Parameter | Rust; Cypher Phase 2 |
| `Array` | `Vec<Value>` | `array<T>` | `[1, 2, 3]` | Implemented |
| `Object` | ordered map (nested documents) | `object` | `{city: 'Hue'}` | Rust; Cypher Phase 2 |
| `Vector` | `Vec<f32>` | `vector<f32, N>` | Parameter | Implemented |
| `Geometry` | Point, LineString, Polygon, MultiPoint, MultiLineString, MultiPolygon, Collection (WGS84) | `point`, `geometry` | `point({longitude: 107.59, latitude: 16.46})`, `geo.polygon(...)` | Rust (validation and WKT display); Cypher and geo functions Phase 3 |
| `RecordId` | table + key | `record<Table>` | `rid('Person', 'alice')`, `id(n)` | Rust; Cypher Phase 2 |
| `Range` | start and end bounds | — | Produced by range-scan APIs | Phase 2 |

Additional DDL type names that are not variants:

- `any` accepts every value.
- `option<T>` accepts `T` or `Null`. In a `SCHEMAFULL` table a field typed `T` must be
  present and non-null; use `option<T>` for optional fields.
- `array<T>` constrains every element to `T`.
- `vector<f32, N>` fixes the dimension, so a write with the wrong dimension is rejected
  with a dimension-mismatch error before it reaches the vector index.
- `record<Table>` only accepts RecordIds whose table is `Table`.

Today's map literals in queries (`{name: 'Alice'}`) are property maps, not values.
With the `Object` variant, a nested map becomes a storable value, which is what turns a
record into a document.

### Comparison and Ordering

- Cypher comparison semantics stay as they are today: `Integer`, `Float` and `Decimal`
  compare numerically across types; comparing with `Null` yields no result; comparing
  unrelated types yields no result.
- Index keys and `ORDER BY` need a total order across types. That order is defined by
  the memcomparable key encoding owned by [Storage Engine](04-storage-engine.md)
  (implemented for record keys), so index order and sort order agree. `Vector` and
  `Geometry` values cannot be part of a key.
- `Datetime` orders chronologically, `Duration` by length, `RecordId` by table and then
  key.

### Rust Shape

The current enum is shown under [Value](#value) below. `Decimal` (128-bit) and
`Range` (start and end bounds) will be appended as new variants in Phase 2. Variants are
only ever appended, so values serialized by earlier versions still decode.

## Implemented Today

`atlantis-core` holds two sets of types. The record model types above (`DatabaseRef`,
`RecordId`, `RecordKey`, `Record`, `Edge`, `Object`) are used by the record store. The
property graph types below are still used by Cypher (`Engine::execute`). In that graph,
nodes and relationships are identified by sequential `u64` IDs, with no tables,
namespaces or RecordIds.

- **Nodes** have zero or more labels and a property map.
- **Relationships** are directed, have exactly one type and a property map.
- The graph is **schemaless**. Labels and relationship types are created implicitly
  on insert.

### Value

Defined in `crates/atlantis-core/src/value.rs`:

```rust
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),      // heterogeneous array
    Vector(Vec<f32>),       // embedding, distinct from Array
    Bytes(Vec<u8>),
    Datetime(Datetime),     // UTC, i64 nanoseconds
    Duration(Duration),     // i64 nanoseconds
    Uuid(Uuid),
    Object(Object),         // BTreeMap<String, Value>, nested documents
    Geometry(Geometry),     // WGS84; Geometry::validate checks bounds and shape
    RecordId(RecordId),     // link to another record
}
```

Cypher only produces and stores the first seven variants today.

`Vector` is separate from `Array` so the vector index can enforce dimensions and store
embeddings compactly.

Comparison behaviour (`PartialEq` / `PartialOrd`):

- `Integer` and `Float` compare across types (`42 == 42.0` is true).
- `Null == Null` is true, but ordering `Null` against any non-null value returns `None`.
- Other mixed-type comparisons are not equal and not ordered.

`From` conversions exist for `bool`, `i64`, `i32` (widened), `f64`, `f32` (widened),
`&str`, `String`, `Vec<f32>` (becomes `Vector`), `Vec<Value>` (becomes `Array`),
`Vec<u8>` (becomes `Bytes`), and each of the newer payload types.
There is no conversion from `Vec<f64>`, so write embedding literals as `f32`
(`vec![0.1f32, 0.2, 0.3]`).

| Method | Returns | Notes |
|---|---|---|
| `as_string()` | `Option<&str>` | `String` only |
| `as_int()` | `Option<i64>` | `Integer` only |
| `as_float()` | `Option<f64>` | `Float`, or `Integer` widened |
| `as_bool()` | `Option<bool>` | `Bool` only |
| `as_vector()` | `Option<&[f32]>` | `Vector` only |
| `as_object()` | `Option<&Object>` | `Object` only |
| `as_record_id()` | `Option<&RecordId>` | `RecordId` only |
| `get_path("a.b")` | `Option<&Value>` | Follows nested objects |
| `is_null()`, `is_vector()` | `bool` | |
| `type_name()` | `&'static str` | `"null"`, `"boolean"`, `"integer"`, `"float"`, `"string"`, `"array"`, `"vector"`, `"bytes"`, `"datetime"`, `"duration"`, `"uuid"`, `"object"`, `"geometry"`, `"record"` |

`Display` renders strings in single quotes (`'hi'`) and arrays and vectors as
`[a, b, c]`.

### NodeId and EdgeId

```rust
pub struct NodeId(pub u64);   // Copy, Eq, Hash, Ord; displays as #42
pub struct EdgeId(pub u64);   // Copy, Eq, Hash, Ord; displays as #e42
```

Both convert from and into `u64`. The storage engine allocates them sequentially, and
they are not stable across a checkpoint restore: restored nodes get new IDs while edges
keep the old ones (fixed in Phase 0). In query results, a node bound to a variable is
returned as its `NodeId` in a `Value::Integer`.

### Node, NodeRef and Relationship

```rust
pub struct Node {
    pub id: NodeId,
    pub labels: LabelSet,
    pub properties: PropertyMap,
}

pub struct NodeRef {           // identity + labels without the property map
    pub id: NodeId,
    pub labels: Arc<LabelSet>,
}

pub struct Relationship {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub rel_type: RelType,
    pub properties: PropertyMap,
}
```

`Node` has builder methods `new`, `with_label`, `with_property` and query methods
`has_label` and `get_property`. `NodeRef::from_node` builds a reference from a node.

### Label, LabelSet and RelType

```rust
pub struct Label(Arc<str>);          // displays as :Person
pub struct LabelSet(BTreeSet<Label>);
pub struct RelType(Arc<str>);        // displays as :KNOWS
```

`Label` and `RelType` offer `new`, `as_str`, `From<&str>` and `From<String>`.
`LabelSet` offers `new`, `insert`, `remove`, `contains`, `len`, `is_empty`, `iter`
(sorted order) and `is_superset`.

### PropertyMap

`PropertyMap` wraps `HashMap<String, Value>` with `new`, `with_capacity`, `insert`,
`get`, `get_mut`, `remove`, `contains`, `len`, `is_empty`, `iter`, `clear`, `extend`
and `FromIterator<(String, Value)>`. Keys are case-sensitive.

### Direction

```rust
pub enum Direction { Outgoing, Incoming, Both }
```

Used by the parser and planner for pattern direction, with `includes_outgoing`,
`includes_incoming` and `reverse`.

### Errors and Serialization

All fallible operations return `Result<T, AtlantisError>`, also named `Error`
(`atlantis_db::Error`). Besides the original graph-engine variants it has record-model
and transaction variants such as `InvalidRecordId`, `RecordNotFound`,
`SchemaViolation` and `TxConflict`. The full list is in
[API Reference](21-api-reference.md#error-and-atlantiserror); conflicts are described in
[Transactions & Consistency](05-transactions-consistency.md).

Core types derive serde `Serialize` and `Deserialize`. The storage crate persists them
with bincode. Configuration types live in `crates/atlantis-core/src/config.rs` and are
documented in [Configuration](22-configuration.md).

## From Cypher's Graph Types to the Record Model

The target types exist; what remains is moving Cypher onto them.

| Cypher today | Record model | Types | Cypher on records |
|---|---|---|---|
| `NodeId(u64)`, sequential | `RecordId` in the node's table, ULID by default | Implemented | Phase 1–2 |
| `EdgeId(u64)`, sequential | `RecordId` in the edge table | Implemented | Phase 1–2 |
| `LabelSet` (sorted set) | First label = table, remaining labels = secondary labels (`Record.labels`) | Implemented | Phase 1–2 |
| `RelType` | Edge table name | Implemented | Phase 1–2 |
| `Relationship.from` / `.to` | `Edge.from` / `.to`, serialized as `in` / `out` | Implemented | Phase 1–2 |
| `PropertyMap` (flat) | Record fields, nested through `Object` | Implemented | Phase 2 |
| `Value` with seven variants in Cypher | Value type system above | Implemented except `Decimal`, `Range` | Phase 2 (geo functions and spatial index: 3) |
| Node returned as an integer ID | Node returned as a record, `id(n)` returns its RecordId | — | Phase 2 |

`LabelSet` sorts labels, so it cannot tell which label was written first. A `Record`
stores its table in its id, separately from the sorted secondary labels, which removes
that ambiguity.

## Design Decisions

**One record model for every data model.** Graph, document, relational, vector,
full-text, geospatial and time-series features all read and write the same records.
Indexes, permissions, change feeds and transactions are built once instead of once per
model.

**First label is the table.** It gives every node a home table for schema, permissions
and key encoding while keeping Cypher's multi-label patterns. Secondary labels stay
cheap through the label index.

**Edges are records, not pointers.** Relationship properties get the same typing,
indexing and permissions as node properties, and an edge can be addressed by its
RecordId.

**RecordId is a value.** That makes record links, foreign-key style references and
joins possible without a second identifier system.

**ULID as the default key.** ULIDs are unique without coordination, which suits a
cluster of stateless compute nodes, and they sort by creation time, which keeps recent
inserts close together in the key space.

**`Vector` stays distinct from `Array`.** Dimension checks and compact `f32` storage
are still needed, and `vector<f32, N>` makes the dimension part of the schema.

**`Arc<str>` labels and `BTreeSet` label sets today.** Labels are cloned often and
passed between actors, so reference-counted strings keep that cheap, and sorted sets
give deterministic iteration and serialization.

## What's Next

- [Storage Engine](04-storage-engine.md) — how records, edges and indexes become keys.
- [Schema & DDL](07-schema-ddl.md) — tables, fields, types, assertions and constraints.
- [Document & Relational](08-document-relational.md) — nested documents, record links and joins.
- [Graph Model](09-graph-model.md) — traversal over edge tables.
- [Implementation Roadmap](25-implementation-roadmap.md) — when each part of this model lands.
