# 28 — Document Model

> **Status:** Review and target design (Phase 2). The foundation is implemented; the gaps and the work plan below are not. Reviewed 2026-11-17 against `main` at `006cd58`.
> **Crates:** `atlantis-core`, `atlantis-kv`, `atlantis-storage`, `atlantis-cypher`, `atlantis-engine`, `atlantis-db`, `atlantis-index` (planned)

What it takes to store, query and update full JSON documents the way people expect
from MongoDB, without giving up standard Cypher. This chapter records a review of the
engine, one design decision, and a work plan that contributors can pick up. The
current document behaviour is described in
[08 — Document & Relational](08-document-relational.md).

## Where We Are

Every node, edge and document is a record: a table, a key and a nested `Object` of
values ([03 — Data Model](03-data-model.md)). That is a real document foundation. What
is missing is the layer that makes it a document database.

| Works today | Missing for a document database |
|---|---|
| Objects and arrays nested to any depth, plus datetime, duration, UUID, bytes, geometry, vector and record-link values | Any JSON import or export |
| Nested reads: `p.address.city`, `p.phones[0].number` | Field order (keys come back sorted) |
| Nested writes: `SET p.a.b = …` creates missing parents; `REMOVE p.a.b` | Paths through arrays and updates to array elements |
| Whole-record merge and replace: `SET p += {…}`, `SET p = {…}` | Atomic update operators (inc, push, pull, addToSet, …) |
| Array membership: `WHERE 'vip' IN p.tags` | Secondary indexes on fields: every filter scans the whole table |
| An order-preserving key encoder for nested values, ready to back indexes | Depth and size limits: deep documents crash the host process |
| A format byte on every stored value, so the format can change safely | A document API alongside Cypher |

## Decision: Two Front Ends, One Engine

**Cypher keeps standard Cypher semantics. MongoDB matching rules live only in the
document API's filter layer.** Both front ends compile to the same plan and share the
same path type, cross-type order, indexes and record storage. Mixing two sets of
matching rules in one query language causes bugs that are hard to see in review.
(Agreed 2026-11-17.)

```
   Cypher                              Document API
   MATCH … WHERE … SET …               find(filter), update_many(filter, ops)
   Cypher equality, null logic,        MongoDB array matching, $exists
   IN for arrays
            \                              /
             └──────── one plan ──────────┘
     Path type · total order · indexes (incl. multikey) · MVCC records
```

Where the two front ends answer differently:

| Question | Cypher | Document API |
|---|---|---|
| Does `tags = 'vip'` match `tags: ['x', 'vip']`? | No. Write `'vip' IN p.tags`. | Yes, `{tags: 'vip'}` matches any element. |
| What is `phones.kind` on an array of objects? | An error today; use `[ph IN p.phones \| ph.kind]`. | The `kind` of every element. |
| Is `{a: null}` different from a missing `a`? | No. Both read as `null`. | Yes. `$exists` tells them apart. |
| `null = null` | `null` (unknown) | `{a: null}` matches null or missing. |

## Findings

Ranked by priority. **Verified** means the behaviour was reproduced against the
engine, not only read from the code.

### P0 — Safety and core semantics

**Deeply nested documents crash the host process** (verified). A document nested
10,000 levels deep aborts the process with a stack overflow instead of returning an
error. Atlantis runs inside the application, so this takes the application down. No
depth or size limit exists. *Fix:* reject over-deep or oversized documents where they
enter (parameters, map literals, `RecordTx::create`, `update`, `merge`), defaulting to
MongoDB's limits of 100 levels and 16 MB. The check must not recurse without a bound.

**Field order is lost** (verified). `Object` is a `BTreeMap`
(`crates/atlantis-core/src/record.rs`), so `{b: 1, a: 2}` comes back as `{a: 2, b: 1}`.
MongoDB keeps insertion order. *Fix:* an insertion-ordered map such as `indexmap`.
Bincode writes both as a length followed by pairs, so stored data should still decode;
a test must prove it. Object equality (`cypher_eq`), `group_key` and object key
encoding (`KeyWriter::value`) assume sorted keys and must sort explicitly. Equality
stays independent of field order.

**Sort order and index key order disagree.** `ORDER BY` (`type_rank` in
`crates/atlantis-engine/src/exec/eval/compare.rs`) ranks object, record, array,
geometry, string, bool, number, …, with null last. The key encoder
(`crates/atlantis-kv/src/encoding.rs`) orders null, bool, number, string, bytes, …,
object. Once an index serves `ORDER BY`, results would depend on whether the index is
used. *Fix:* one total order across types in `atlantis-core`, used by both.

**The sort comparator is not a total order.** `order_cmp` falls back to "equal" for
objects, vectors containing NaN and objects with mixed-type fields, and it feeds
`sort_by`, which since Rust 1.81 may panic when a comparator breaks total ordering.
`impl Eq for Value` is unsound because NaN is not equal to itself. *Fix:* the shared
total order above, with an explicit NaN rule.

### P1 — Document behaviour

**No JSON import or export.** There is no `serde_json` in the workspace, and
`Display` prints a Cypher-like form, not JSON. *Fix:* `Value ⇄ JSON` in
`atlantis-core` behind a `json` feature, with canonical forms for types JSON lacks,
modelled on MongoDB Extended JSON: `{"$date": …}`, `{"$uuid": …}`, `{"$binary": …}`,
`{"$rid": "Person:alice"}`, and GeoJSON for geometry. Integers that fit in `i64`
become `Integer`. JSON output for result sets, and `toJson()` / `fromJson()`.

**Paths cannot step through arrays or address elements** (verified):

```cypher
RETURN p.phones.kind              // error: cannot read property kind of a array
SET p.phones[0].number = '9'      // parse error
SET p.address += {zip: '53'}      // parse error
```

*Fix:* one `Path` type in core with field and index segments, used by reads,
`set_path`, the `WHERE` push-down and indexes. Cypher gains element updates and nested
merge. Fanning out over arrays belongs to the document API only.

**Dots in field names are ambiguous** (verified). ``SET p.`a.b` = 1`` stores a literal
key `"a.b"`, which `Value::get_path` can never reach because it splits on `.`. *Fix:*
the `Path` type, and reject `.` and a leading `$` in field names as MongoDB does.

**JSON null does not survive a round trip.** A null write removes the field, `SET p =
{…}` filters nulls out and `merge` removes them, so `{"a": null}` comes back as `{}`
after any update. See [Open Questions](#open-questions).

**No atomic update operators.** Every write reads the whole document, edits it in the
executor and writes it back. *Fix:* `RecordTx::patch(id, &[UpdateOp])` with set,
unset, inc, mul, min, max, push, pull, addToSet, pop, rename and positional updates.
Cypher `SET` and the document API both compile to it.

**The `id` field is taken as the record key.** `take_key`
(`crates/atlantis-storage/src/records/crud.rs`) turns an `id` field into the key, so a
JSON import where `id` is a business field silently changes the key, and an object
`id` is rejected. *Fix:* JSON import takes an option for how ids map to keys.

### P2 — Indexes and speed

**No secondary indexes on fields.** The key layout (`crates/atlantis-kv/src/keys.rs`)
holds records, adjacency and labels only. Every `WHERE p.x = …` loads the whole table
and filters it. *Fix:* `atlantis-index` as designed in
[08 § Secondary Indexes](08-document-relational.md#secondary-indexes-planned--phase-2),
plus multikey and partial indexes, and nested-path push-down for filtered vector search
(today only top-level `node.field`).

**Each property read decodes the whole document again.** Reading a property of a node
(`property()` in `crates/atlantis-engine/src/exec/eval/access.rs`) does a KV lookup and
a full decode every time; `RETURN p.a, p.b, p.c` decodes three times, after the scan
already decoded the record and kept only its id. *Fix:* a per-statement record cache,
or keep the decoded record in the row.

**Scans load everything before filtering.** Table scans return a `Vec`, so `LIMIT 10`
over a million documents decodes all of them. *Fix:* streaming cursors with limit
push-down.

**Field-level reads from storage (optional).** A binary format with field offsets
(like BSON or JSONB) would allow partial reads. Only if profiling after the record
cache says so.

### P3 — API

**No document API.** The only entry points are `execute(cypher)` and raw record CRUD.
*Fix:* a Rust collection API with typed builders compiled to the same plan as Cypher,
then the Python package:

```rust
let orders = db.collection("Order");
orders.insert_one(json!({"customer": "ana", "items": [{"sku": "A1", "qty": 2}]}))?;
orders.update_many(
    Filter::eq("items.sku", "A1"),          // matches any array element
    Update::inc("items.$.qty", 1),
)?;
```

**Map projection is not supported** (verified). `RETURN p {.name, .address}` is a
parse error. It is Cypher's equivalent of a MongoDB projection.

## Work Plan

In dependency order. Sizes are relative: S is days, M about a week, L several weeks.

| # | Package | Size | Crates | Done when |
|---|---|---|---|---|
| 1 | **Depth and size limits.** Iterative validator, configurable, default 100 levels and 16 MB; applied to parameters, map literals and every record write path | S | core, storage, engine | A 10,000-level document returns an error and the process keeps running |
| 2 | **Core document semantics.** Insertion-ordered `Object` with a decode-compatibility test; `Path` type and field-name rules; one total order across types used by `ORDER BY`, `min`/`max` and the key encoder; NaN rule and `Eq` fix; `Value ⇄ JSON` | M | core, kv, engine | A JSON corpus round-trips exactly, and sorting mixed-type values matches index order |
| 3 | **Update operators.** `RecordTx::patch`; Cypher `SET p.phones[0].n = …` and nested `SET p.address += {…}` | M | storage, cypher, engine | The "Planned" row in [08 § Writing nested values](08-document-relational.md#writing-nested-values) is implemented |
| 4 | **Record cache and streaming scans.** One decode per record per statement; iterator scans with limit push-down | M | storage, engine | A benchmark shows one decode per record and `LIMIT 10` stops early |
| 5 | **Field indexes.** `atlantis-index` under `/ix {name}` with the existing encoder; single-field, compound, multikey, nested-path, unique, partial; `CREATE INDEX` / `CREATE CONSTRAINT`; planner seek and range scan; `EXPLAIN` | L | `atlantis-index` (new), kv, cypher, engine | `EXPLAIN` shows an index seek for equality and range filters on nested and array fields |
| 6 | **Document API.** `Filter` evaluator with MongoDB rules (`$exists`, `$elemMatch`, `$in`, ranges, array matching); insert, find with sort/limit/skip/projection, update, delete, count; shares transactions with Cypher | L | engine, `atlantis-db`, Python package | A conformance suite of filter cases from MongoDB's documented behaviour passes |

## Open Questions

| Question | Suggested answer |
|---|---|
| Should stored documents keep explicit nulls? | Keep them when they arrive through the document API or JSON import; keep the Cypher rule (null write removes) for Cypher writes. |
| Which cross-type order becomes canonical? | The key encoder's order (null, bool, number, string, bytes, datetime, duration, uuid, array, object, record), because composite record keys are already stored with it. |
| How are large and exact numbers handled? | Land the planned 128-bit `Decimal` with the JSON work, and import integers beyond `i64` as `Decimal` rather than a lossy `Float`. |
| Ship a parser for MongoDB's JSON query syntax? | Not in Phase 2. Typed builders first; revisit once the Python package has users. |

## Contributing

Pick a work package and open an issue that names it so others can see it is taken.
Keep each pull request to one package or less. Packages 1 and 2 have no dependencies.
Every finding above should end up as a test.

## What's Next

- **[08 — Document & Relational](08-document-relational.md)** — Today's document behaviour.
- **[03 — Data Model](03-data-model.md)** — Records and the value type system.
- **[07 — Schema & DDL](07-schema-ddl.md)** — Fields, types and index DDL.
- **[25 — Implementation Roadmap](25-implementation-roadmap.md)** — Phase 2.
