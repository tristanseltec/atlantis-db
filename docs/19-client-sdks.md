# 19 — Client SDKs

> **Status:** Partially implemented — embedded Rust API only; remote and other SDKs Planned — Phase 6
> **Crates:** `atlantis-db`, `atlantis-server` (planned); SDK packages `atlantisdb` (Python), `atlantisdb.js` (JS/TS + WASM), Go and Java SDKs (planned)

This chapter describes how applications connect to Atlantis DB from Rust, Python,
JavaScript/TypeScript (including an embedded WebAssembly build), Go and Java. Every SDK
exposes the same method surface as the WebSocket RPC protocol in
[17-server-protocols.md](17-server-protocols.md), so a concept learned in one language
carries over to the others.

## Implemented Today

Only the embedded Rust API exists. It lives in `crates/atlantis-db/src/engine.rs`:

```rust
use atlantis_db::{Engine, Value};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::open("./data").await?;          // or Engine::open_in_memory()
    let rs = engine
        .execute("MATCH (p:Person) WHERE p.age > $min RETURN p.name AS name",
                 &[("min", Value::Integer(30))])
        .await?;
    for row in rs.rows() {
        println!("{:?}", row.get_string("name"));
    }
    engine.shutdown().await?;
    Ok(())
}
```

`Engine` also offers `open_with_config`, `checkpoint`, `shutdown`, and the record API
(`transaction`, `transaction_with_retry`, `read`, `use_db`, and Cypher inside a
transaction with `tx.execute`; see [21-api-reference.md](21-api-reference.md)). Errors
are `atlantis_db::Error`, an alias of `AtlantisError`. There are no connection URLs, no
remote connections, no authentication, and no other language SDKs.
The full current surface is in [21-api-reference.md](21-api-reference.md).

The rest of this chapter is the **target design** for Phase 6. The Rust remote
connection depends on the server from Phase 5.

## Connection URLs (Planned — Phase 6)

| URL | Mode | Available in |
|---|---|---|
| `mem://` | Embedded, in-memory | Rust, JS/TS (WASM), Python and Java via native bindings |
| `rocksdb://path` | Embedded, persistent | Rust; Python and Java via native bindings |
| `ws://host:7700`, `wss://host:7700` | Remote, WebSocket RPC session | All SDKs |
| `http://host:7700`, `https://host:7700` | Remote, stateless HTTP | All SDKs |
| `tikv://pd1:2379,pd2:2379` | Embedded compute node over a TiKV cluster | Rust only |

`ws://` and `wss://` are the recommended remote schemes. They support live queries,
session variables and session transactions. `http://` suits serverless functions and
short-lived jobs; over HTTP, `live` is unavailable and `begin`/`commit` are emulated by
sending the whole transaction as one `BEGIN; ... COMMIT;` query. `tikv://` turns the
application process itself into a cluster compute node (see
[20-distributed-architecture.md](20-distributed-architecture.md)); most applications
should instead connect to `atlantis` servers.

Embedded `mem://` and `rocksdb://` connections run with the implicit full-privilege
session described in [18-security-auth.md](18-security-auth.md); `signin` is a no-op
there.

## Shared Method Surface (Planned — Phase 6)

Each SDK maps RPC methods to idiomatic names. Results are decoded into the language's
native types using the [type mapping](#type-mapping-planned--phase-6) below.

| RPC method | Rust | Python | JS/TS | Go | Java |
|---|---|---|---|---|---|
| connect | `Engine::connect(url)` | `Atlantis(url)` | `db.connect(url)` | `atlantis.Connect(ctx, url)` | `Atlantis.connect(url)` |
| `use` | `use_db(ns, db)` | `use(ns, db)` | `use({namespace, database})` | `Use(ctx, ns, db)` | `use(ns, db)` |
| `signup` / `signin` | `signup` / `signin` | same | same | `Signup` / `Signin` | same |
| `authenticate` / `invalidate` | same | same | same | `Authenticate` / `Invalidate` | same |
| `query` | `execute(cypher, params)` | `query` | `query` | `Query` | `query` |
| `select`, `create`, `insert`, `update`, `merge`, `patch`, `delete`, `relate` | same names | same | same | capitalized | same |
| `live` / `kill` | `live` (stream) / `kill` | async iterator / `kill` | callback or async iterator / `kill` | channel / `Kill` | `Flow.Publisher` / `kill` |
| `let` / `unset` | `set_var` / `unset_var` | `let` / `unset` | `let` / `unset` | `Let` / `Unset` | `let` / `unset` |
| `begin` / `commit` / `cancel` | `transaction(closure)`, `transaction_with_retry(n, closure)` | `async with db.transaction()` | `db.transaction(async tx => ...)` | `Transaction(ctx, func)` | `transaction(tx -> ...)` |

Rust keeps the name `execute` for queries because that is the method the embedded
`Engine` already exposes. `use` is a reserved word in Rust, so the method is `use_db`;
`let` is reserved too, so session variables use `set_var`.

Every SDK sends query parameters separately from the query text. String concatenation
into Cypher is never needed and is the main injection risk listed in
[18-security-auth.md](18-security-auth.md).

## Rust (Planned — Phase 6)

The `atlantis-db` crate keeps one `Engine` type for embedded and remote connections.
Cargo features choose what is compiled in: `kv-mem` and `kv-rocksdb` for embedded,
`kv-tikv` for cluster compute, and `protocol-ws` / `protocol-http` for remote clients.
Application code does not change between modes; only the URL does.

```rust
use atlantis_db::{Engine, Value, Credentials, Error};
use futures::StreamExt;

let engine = Engine::connect("wss://db.example.com").await?;   // or "rocksdb://data"
engine.signin(Credentials::root("admin", &pw)).await?;
engine.use_db("acme", "shop").await?;

let rs = engine
    .execute("MATCH (o:Order) WHERE o.total > $min RETURN o", &[("min", Value::from(100))])
    .await?;

// Live query: a Stream of notifications; dropping the stream kills the live query
let mut live = engine.live("LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o", &[]).await?;
while let Some(n) = live.next().await {
    let n = n?;
    println!("{:?} {:?}", n.action, n.record);
}

// Transaction: commits once, or returns Err(Error::TxConflict) without retrying.
// The closure is synchronous (implemented today); tx.execute for Cypher inside a
// transaction is Phase 2 — today the closure uses RecordTx methods.
engine.transaction(|tx| {
    tx.execute("MATCH (a:Account {id: $from}) SET a.balance = a.balance - $amt", &[/* ... */])?;
    tx.execute("MATCH (a:Account {id: $to})   SET a.balance = a.balance + $amt", &[/* ... */])?;
    Ok(())
}).await?;

// Same body, re-run up to 5 times on Error::TxConflict
engine.transaction_with_retry(5, |tx| {
    tx.execute("MATCH (a:Account {id: $id}) SET a.balance = a.balance + $amt", &[/* ... */])?;
    Ok(())
}).await?;
```

`transaction` returns the closure's `Result<T>`. Record users sign in with `Credentials::record("acme", "shop", "account", params)`.
Existing `Engine::open(path)` and `Engine::open_in_memory()` remain as shorthands for
`rocksdb://path` and `mem://`.

## Python (Planned — Phase 6)

Package `atlantisdb`, async-first with a synchronous wrapper for scripts.

```python
from atlantisdb import Atlantis, TxConflict

async with Atlantis("ws://localhost:7700") as db:
    await db.signin({"user": "admin", "pass": pw})
    await db.use("acme", "shop")

    rows = await db.query(
        "MATCH (p:Person) WHERE p.age >= $min RETURN p.name AS name", {"min": 30}
    )
    for row in rows[0]:
        print(row["name"])

    async with db.transaction() as tx:
        await tx.query("CREATE (:Order {total: $t, status: 'open'})", {"t": 129.5})

    async for n in db.live("LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o"):
        print(n.action, n.record)
```

`db.query` returns one result list per statement. `from atlantisdb import AtlantisSync`
offers the same methods without `await`.

## JavaScript and TypeScript (Planned — Phase 6)

Package `atlantisdb.js`, written in TypeScript, for browsers, Node.js, Deno and Bun.

```ts
import { Atlantis, RecordId, TxConflict } from "atlantisdb.js";

const db = new Atlantis();
await db.connect("wss://db.example.com");
await db.signin({ namespace: "acme", database: "shop", access: "account",
                  variables: { email, pass } });
await db.use({ namespace: "acme", database: "shop" });

const [people] = await db.query<{ name: string }>(
  "MATCH (p:Person) WHERE p.age >= $min RETURN p.name AS name", { min: 30 });

const liveId = await db.live(
  "LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o",
  ({ action, record }) => console.log(action, record));
// later
await db.kill(liveId);

await db.transaction(async (tx) => {
  await tx.query("CREATE (:Order {total: $t, status: 'open'})", { t: 129.5 });
});
```

Generic type parameters on `query` type the rows; they are not validated at runtime.

### Embedded WebAssembly build

`atlantisdb.js/wasm` compiles the Rust engine to WebAssembly with the `kv-mem` backend.
It runs entirely inside the browser tab or a worker, with no server:

```ts
import { Atlantis } from "atlantisdb.js";
import { wasmEngine } from "atlantisdb.js/wasm";

const db = new Atlantis({ engines: wasmEngine() });
await db.connect("mem://");
await db.query("CREATE (:Note {text: $t, embedding: $v})", { t: "hello", v: embedding });
```

Use cases are offline-first apps, local agent memory, and tests. Data lives in memory and
is lost when the page closes; persistent browser storage for the WASM build is an open
design question.

## Go (Planned — Phase 6)

Module path to be assigned under the `atlantis-db` GitHub organization. Methods take a
`context.Context` for cancellation and deadlines.

```go
db, err := atlantis.Connect(ctx, "ws://localhost:7700")
if err != nil { return err }
defer db.Close()

if _, err := db.Signin(ctx, atlantis.RootAuth{User: "admin", Pass: pw}); err != nil { return err }
if err := db.Use(ctx, "acme", "shop"); err != nil { return err }

res, err := db.Query(ctx, "MATCH (p:Person) WHERE p.age >= $min RETURN p.name AS name",
    map[string]any{"min": 30})

notes, err := db.Live(ctx, "LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o", nil)
for n := range notes { fmt.Println(n.Action, n.Record) }

err = db.Transaction(ctx, func(tx *atlantis.Tx) error {
    _, err := tx.Query(ctx, "CREATE (:Order {total: $t})", map[string]any{"t": 129.5})
    return err
})
```

`atlantis.Unmarshal(res[0].Rows, &dst)` decodes rows into structs with `atlantis:"name"`
field tags.

## Java (Planned — Phase 6)

Artifact coordinates to be assigned. The client is thread-safe and returns
`CompletableFuture` results.

```java
try (Atlantis db = Atlantis.connect("wss://db.example.com")) {
    db.signin(Auth.root("admin", pw)).join();
    db.use("acme", "shop").join();

    List<Result> res = db.query(
        "MATCH (p:Person) WHERE p.age >= $min RETURN p.name AS name",
        Map.of("min", 30)).join();

    db.live("LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o")
      .subscribe(n -> System.out.println(n.action() + " " + n.record()));

    db.transaction(tx -> tx.query("CREATE (:Order {total: $t})", Map.of("t", 129.5))).join();
}
```

Kotlin users get the same client; coroutine extensions are a possible later addition.

## Type Mapping (Planned — Phase 6)

SDKs use CBOR on the wire so every `Value` type round-trips exactly (see the JSON and CBOR
section of [17-server-protocols.md](17-server-protocols.md)). The `Value` type system is
owned by [03-data-model.md](03-data-model.md).

| `Value` | Rust | Python | JS/TS | Go | Java |
|---|---|---|---|---|---|
| `Null` | `Value::Null` / `Option::None` | `None` | `null` | `nil` | `null` |
| `Bool` | `bool` | `bool` | `boolean` | `bool` | `Boolean` |
| `Integer` | `i64` | `int` | `bigint` outside the safe-integer range, else `number` | `int64` | `Long` |
| `Float` | `f64` | `float` | `number` | `float64` | `Double` |
| `Decimal` | `rust_decimal::Decimal` | `decimal.Decimal` | `Decimal` (SDK class) | `atlantis.Decimal` | `BigDecimal` |
| `String` | `String` | `str` | `string` | `string` | `String` |
| `Bytes` | `Vec<u8>` | `bytes` | `Uint8Array` | `[]byte` | `byte[]` |
| `Datetime` | `Datetime` (UTC, ns) | `datetime` (UTC; ns kept in SDK type) | `DateTime` (SDK class; ns precision) | `time.Time` | `Instant` |
| `Duration` | `Duration` | `timedelta` | `Duration` (SDK class) | `time.Duration` | `java.time.Duration` |
| `Uuid` | `uuid::Uuid` | `uuid.UUID` | `Uuid` (SDK class) | `atlantis.UUID` | `java.util.UUID` |
| `Array` | `Vec<Value>` | `list` | `Array` | `[]any` | `List<Object>` |
| `Object` | `Object` (ordered map) | `dict` | `object` | `map[string]any` / struct | `Map<String,Object>` (ordered) |
| `Vector` | `Vec<f32>` | `list[float]` or `numpy.ndarray` (float32) | `Float32Array` | `[]float32` | `float[]` |
| `Geometry` | `Geometry` | GeoJSON-shaped `dict` / `Geometry` class | GeoJSON object | `atlantis.Geometry` | `Geometry` |
| `RecordId` | `RecordId` | `RecordId` | `RecordId` | `atlantis.RecordID` | `RecordId` |
| `Range` | `Range` | `Range` | `Range` | `atlantis.Range` | `Range` |

Rules that apply in every SDK:

- `RecordId` is a distinct type, never a plain string, so that `"Person:alice"` the
  string and `Person:alice` the record id stay distinct. Each SDK offers a constructor
  such as `RecordId("Person", "alice")` and parsing from text.
- Integers larger than the host language can represent exactly are never silently
  rounded; JS uses `bigint` for them.
- Datetimes are always UTC on the wire. SDKs convert to local time only on request.

## Error Handling and Retries (Planned — Phase 6)

Errors carry the stable code from the server (`TxConflict`, `PermissionDenied`,
`ParseError`, `AuthenticationFailed`, `NotFound`, `Timeout`, ...) and the message. Each
SDK exposes the code as a typed error: `atlantis_db::Error::TxConflict` in Rust,
`atlantisdb.TxConflict` in Python, `TxConflict` class in JS/TS, `errors.Is(err,
atlantis.ErrTxConflict)` in Go, `TxConflictException` in Java.

### Retrying `TxConflict`

Atlantis DB uses optimistic concurrency (see
[05-transactions-consistency.md](05-transactions-consistency.md)). A transaction that
loses a write conflict fails at commit with `TxConflict` and has no effect, so it is safe
to run again.

Retrying is explicit. `transaction(...)` in every SDK runs the body once and surfaces
`TxConflict` to the caller. The retrying variant (`transaction_with_retry(max_attempts,
f)` in Rust, and a `retries` option on `transaction(...)` in the other SDKs) re-runs the
body on conflict:

- the whole closure is re-run, so it must not have side effects outside the database
  (no emails, no external HTTP calls) or those must be idempotent;
- exponential backoff with jitter between attempts, starting at a few milliseconds, up to
  the given maximum number of attempts;
- only `TxConflict` is retried; every other error is returned immediately.

A single `query` call that contains `BEGIN; ... COMMIT;` is not retried automatically,
because the SDK cannot know whether the caller's surrounding logic depends on it. Wrap it
in a retrying transaction to get retries.

### Live Query Overflow and Connection Failures

Each live query has a bounded notification queue on the server. A consumer that falls
too far behind has its live query killed; the SDK ends the stream, iterator or
subscription with a typed "live query killed" error. The application then catches up
through `db.changes` and registers a new live query (see
[14-live-queries-events.md](14-live-queries-events.md)).


WebSocket SDKs reconnect with backoff after a dropped connection, re-run `use`, and
re-authenticate with the stored token while it is valid. Live queries are re-registered,
and a `reconnected` event tells the application that notifications may have been missed
during the gap; applications that cannot miss a change should reconcile through
`db.changes` (see [14-live-queries-events.md](14-live-queries-events.md)). An open
session transaction is lost on disconnect and surfaces as an error to its caller; it is
never silently replayed.

## Design Decisions

- **RPC surface as the SDK contract.** One protocol definition, five thin SDKs, and a
  shared vocabulary across languages.
- **Same `Engine` for embedded and remote in Rust.** Applications can start embedded and
  move to a server by changing a URL.
- **CBOR by default.** Exact type round-trips matter more than human-readable frames.
- **Explicit retries.** Retrying is opt-in and only re-runs a body the SDK owns
  completely, because re-running a body with outside side effects is unsafe.

## What's Next

- [17-server-protocols.md](17-server-protocols.md) — the RPC protocol under every SDK
- [18-security-auth.md](18-security-auth.md) — sign-in levels and access methods
- [21-api-reference.md](21-api-reference.md) — the Rust API in detail
- [24-rag-patterns.md](24-rag-patterns.md) — application patterns using these SDKs
