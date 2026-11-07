# 17 — Server and Protocols

> **Status:** Planned — Phase 5 (Bolt: Phase 6)
> **Crates:** `atlantis-server` (planned), `atlantis-auth` (planned), `atlantis-engine`, `atlantis-db`

This chapter describes the standalone `atlantis` server and its command-line tool: the
HTTP REST API, the WebSocket RPC protocol, the optional Bolt listener, import and
export, backup and restore, health and metrics, and how sessions carry namespace,
database and authentication state.

## Implemented Today

Nothing in this chapter is implemented. Atlantis DB is an embedded library today: an
application opens a database with `Engine::open(path)` or `Engine::open_in_memory()`
and runs Cypher through `Engine::execute(script, params)` or record transactions in the
same process (see
`crates/atlantis-db/src/engine.rs` and [21-api-reference.md](21-api-reference.md)).
There is no network listener, no binary, and no authentication.

Everything below is the **target design** for Phase 5, with the Bolt listener in Phase 6.

## Deployment Shape (Planned — Phase 5)

The server is a thin protocol layer. It owns sockets, sessions, serialization and the
authentication handshake, and it delegates every statement to `atlantis-engine`, which
enforces transactions and permissions. The embedded `Engine` and the server run the same
engine code; only the transport differs.

```
 HTTP client ─┐
 WS RPC client├─▶ atlantis-server ──▶ atlantis-auth (verify credentials / tokens)
 Bolt driver ─┘        │
                       ▼
                 atlantis-engine (session, tx, permissions, executor, live queries)
                       │
                 atlantis-kv backend: mem:// | rocksdb://path | tikv://pd:2379
```

A single `atlantis` process is the **Server** deployment mode. Several `atlantis`
processes over one `tikv://` cluster form the **Cluster** mode described in
[20-distributed-architecture.md](20-distributed-architecture.md).

## The `atlantis` Binary (Planned — Phase 5)

| Subcommand | Purpose |
|---|---|
| `start` | Run the server over a storage URL |
| `shell` | Interactive Atlantis Cypher shell against a server or a local datastore |
| `import` | Run a Cypher script file against a database |
| `export` | Write a database as a Cypher script file |
| `backup` | Take a physical, consistent snapshot of a datastore |
| `restore` | Restore a datastore from a backup |
| `upgrade` | Migrate an on-disk datastore to the storage format of this binary |
| `version` | Print the binary version, or a server's version with `--endpoint` |
| `isready` | Exit 0 when a server answers `GET /health`, non-zero otherwise |

### `atlantis start`

```bash
atlantis start --bind 0.0.0.0:7700 --user root --pass "$PW" rocksdb://data
```

The positional argument is a storage URL. Without one the server uses `mem://`, which
loses all data on exit and prints a warning.

| Flag | Default | Meaning |
|---|---|---|
| `--bind` | `127.0.0.1:7700` | Listen address for HTTP and WebSocket RPC |
| `--user`, `--pass` | none | Create this root user with role `owner` on first start if no root user exists |
| `--tls-cert`, `--tls-key` | none | PEM certificate chain and private key; enables HTTPS and WSS on the same port |
| `--allow-origin` | none | Comma-separated CORS origins; `*` allows any origin |
| `--bolt` | off | Enable the Bolt listener (binary built with feature `bolt`, Phase 6) |
| `--bolt-bind` | `127.0.0.1:7687` | Bolt listen address |
| `--log` | `info` | Log level: `error`, `warn`, `info`, `debug`, `trace` |
| `--audit-log` | none | Path for the audit log (see [18-security-auth.md](18-security-auth.md)) |
| `--insecure-no-auth` | off | Disable authentication; refused unless `--bind` is a loopback address |

Server-wide settings live in `atlantis.toml`, described in
[22-configuration.md](22-configuration.md). The keys this chapter relies on are
`storage.url`, `storage.sync` (`every` | `interval` | `never`), `transaction.isolation`
(`snapshot` | `serializable`) and `query.timeout` (default `30s`). Command-line flags
override the file.

The default bind address is loopback on purpose. Exposing the server requires an
explicit `--bind 0.0.0.0:7700`, and a server that binds a non-loopback address without
TLS logs a warning at startup.

### `atlantis shell`

```bash
atlantis shell --endpoint ws://localhost:7700 --ns acme --db shop --user admin --pass "$PW"
atlantis shell --endpoint rocksdb://data            # open a local datastore directly
```

The shell sends each statement through the RPC `query` method and prints results as
tables. Multi-statement input, `BEGIN; ... COMMIT;` blocks and `USE` work as they do in
any session.

### `atlantis import` and `atlantis export`

```bash
atlantis export --endpoint https://db.example.com --ns acme --db shop \
  --user admin --pass "$PW" shop.cypher
atlantis import --endpoint ws://localhost:7700 --ns acme --db shop \
  --user admin --pass "$PW" shop.cypher
```

Both commands go through the server's `GET /export` and `POST /import` endpoints, so they
obey the caller's permissions. The file format is described in
[Import and Export](#import-and-export-planned--phase-5).

### `atlantis backup`, `restore`, `upgrade`

```bash
atlantis backup  rocksdb://data ./backups/2026-11-07
atlantis restore ./backups/2026-11-07 rocksdb://data-restored
atlantis upgrade rocksdb://data
```

These commands operate on the datastore, not through a session. See
[Backup and Restore](#backup-and-restore-planned--phase-5).

### Environment Variables

Configuration keys map to environment variables by upper-casing the key, replacing `.`
with `_` and adding the `ATLANTIS_` prefix: `ATLANTIS_STORAGE_URL` (the positional
storage URL), `ATLANTIS_STORAGE_SYNC`, `ATLANTIS_TRANSACTION_ISOLATION` and
`ATLANTIS_QUERY_TIMEOUT`. `start` flags follow the same pattern, for example
`ATLANTIS_BIND`, `ATLANTIS_USER` and `ATLANTIS_PASS`. Passing secrets through the environment or a secrets manager keeps them
out of shell history and process listings.

## HTTP REST API (Planned — Phase 5)

All endpoints are served on the main port (default **7700**).

### Common Headers

| Header | Purpose |
|---|---|
| `Authorization: Bearer <token>` | Token returned by `/signin` or `/signup`, or an external JWT accepted by a `TYPE JWT` access method |
| `Authorization: Basic <base64>` | System-user credentials, for scripts and tools |
| `Atlantis-NS` | Namespace for this request |
| `Atlantis-DB` | Database for this request |
| `Accept` | `application/json` (default) or `application/cbor` |
| `Content-Type` | `application/json`, `application/cbor`, or `text/plain` for `/query` and `/import` bodies |

HTTP is stateless. Every request carries its own credentials and its own
namespace/database; nothing persists between requests except what the token encodes.

### Endpoints

| Method and path | Purpose |
|---|---|
| `POST /query` | Run one or more Atlantis Cypher statements with parameters |
| `GET /key/{table}` | List records of a table |
| `POST /key/{table}` | Create a record with a generated key |
| `GET /key/{table}/{id}` | Read one record |
| `POST /key/{table}/{id}` | Create a record with this key |
| `PUT /key/{table}/{id}` | Replace a record |
| `PATCH /key/{table}/{id}` | Merge fields into a record |
| `DELETE /key/{table}[/{id}]` | Delete one record, or all records the caller may delete |
| `POST /signup` | Sign up through a `TYPE RECORD` access method |
| `POST /signin` | Sign in as a system user or through an access method |
| `GET /health` | Liveness and storage reachability |
| `GET /version` | Server version |
| `GET /metrics` | Prometheus metrics |
| `POST /import` | Run an uploaded Cypher script |
| `GET /export` | Download the current database as a Cypher script |

### `POST /query`

A JSON body carries the statements and their parameters:

```bash
curl -X POST https://db.example.com/query \
  -H "Authorization: Bearer $TOKEN" \
  -H "Atlantis-NS: acme" -H "Atlantis-DB: shop" \
  -H "Content-Type: application/json" \
  -d '{
        "query": "MATCH (p:Person) WHERE p.age >= $min RETURN p.name AS name, p.age AS age ORDER BY age",
        "params": {"min": 30}
      }'
```

The response has one entry per statement, in order:

```json
{
  "results": [
    {
      "status": "OK",
      "time": "1.84ms",
      "columns": ["name", "age"],
      "rows": [["Alice", 31], ["Bob", 42]]
    }
  ]
}
```

A failing statement reports an error with a stable code. When the statements are inside
`BEGIN; ... COMMIT;`, a failure rolls back the whole block and every statement of the
block reports the error:

```json
{
  "results": [
    {"status": "ERR", "time": "0.40ms",
     "error": {"code": "TxConflict", "message": "write conflict on Order:1042; retry the transaction"}}
  ]
}
```

The HTTP status is `200` whenever the request was parsed and executed, even if a
statement failed. Transport and authentication problems use HTTP status codes: `400`
malformed body, `401` missing or invalid credentials, `403` the authenticated identity
may not use this namespace or database, `413` body too large, `503` storage unavailable.

### `/key` Record Endpoints

The record endpoints are shortcuts for single-table operations. They run the equivalent
Cypher through the same engine path, so table permissions apply.

```bash
# Create Person:alice
curl -X POST http://localhost:7700/key/Person/alice \
  -H "Authorization: Bearer $TOKEN" -H "Atlantis-NS: acme" -H "Atlantis-DB: shop" \
  -H "Content-Type: application/json" \
  -d '{"name": "Alice", "address": {"city": "Hue"}}'

# Merge one field
curl -X PATCH http://localhost:7700/key/Person/alice ... -d '{"address": {"city": "Da Nang"}}'
```

```json
{"id": "Person:alice", "name": "Alice", "address": {"city": "Da Nang"}}
```

`GET /key/{table}` accepts `limit` and `start` query parameters for paging. Anything more
selective belongs in `POST /query`.

### `/signup` and `/signin`

```bash
curl -X POST http://localhost:7700/signin -H "Content-Type: application/json" \
  -d '{"ns": "acme", "db": "shop", "access": "account",
       "email": "an@example.com", "pass": "correct horse battery staple"}'
```

```json
{"token": "eyJhbGciOi..."}
```

The body names the target level: no `ns`/`db` for a root user, `ns` only for a
namespace user, `ns` + `db` for a database user, and `ns` + `db` + `access` for a record
access method. All other body fields become parameters of the access method's `SIGNIN`
or `SIGNUP` block. The semantics are in [18-security-auth.md](18-security-auth.md).

### `/health` and `/version`

`GET /health` returns `200` with `{"status": "ok"}` when the server can read from its
storage backend, and `503` otherwise. It needs no authentication and reveals no data, so
load balancers and orchestrators can poll it. `GET /version` returns
`{"version": "x.y.z"}`.

## WebSocket RPC (Planned — Phase 5)

The WebSocket endpoint is `/rpc` on the main port. A WebSocket connection is a
**session**: it keeps its namespace, database, authentication, session variables, live
queries and an optional open transaction until the socket closes.

### Envelope

Each request is one message:

```json
{"id": 7, "method": "query", "params": ["MATCH (p:Person {name: $n}) RETURN p", {"n": "Alice"}]}
```

`id` is chosen by the client (integer or string) and echoed back. A success response
carries `result`; a failure carries `error`:

```json
{"id": 7, "result": [{"status": "OK", "time": "0.9ms", "columns": ["p"], "rows": [[{"id": "Person:alice", "name": "Alice"}]]}]}
{"id": 8, "error": {"code": "PermissionDenied", "message": "create on Post is not permitted"}}
```

Requests on one connection may be pipelined. Responses may arrive out of order and are
matched by `id`. Statements within one session that is inside an explicit transaction are
executed in request order.

### Methods

| Method | Params | Result |
|---|---|---|
| `use` | `[ns, db]` (either may be `null` to keep the current value) | `null` |
| `signup` | `[{ns, db, access, ...vars}]` | token |
| `signin` | `[{user, pass}]` or `[{ns, db, access, ...vars}]` | token |
| `authenticate` | `[token]` | `null`; the session adopts the token's identity |
| `invalidate` | `[]` | `null`; the session becomes anonymous |
| `info` | `[]` | the `$auth` record or system-user description |
| `query` | `[cypher, params?]` | array of statement results, as in `POST /query` |
| `select` | `[table \| recordId]` | records |
| `create` | `[table \| recordId, data?]` | created record |
| `insert` | `[table, data \| [data]]` | inserted records |
| `update` | `[table \| recordId, data?]` | replaced records |
| `merge` | `[table \| recordId, data]` | records with `data` merged in |
| `patch` | `[table \| recordId, jsonPatchOps]` | patched records (RFC 6902 operations) |
| `delete` | `[table \| recordId]` | deleted records |
| `relate` | `[fromId, edgeTable, toId, data?]` | created edge record |
| `live` | `[cypher, params?]` | live query id (a `LIVE MATCH ...` statement) |
| `kill` | `[liveId]` | `null` |
| `let` | `[name, value]` | `null`; defines `$name` for later statements in this session |
| `unset` | `[name]` | `null` |
| `begin` | `[]` | `null`; opens a session transaction |
| `commit` | `[]` | `null`, or a `TxConflict` error |
| `cancel` | `[]` | `null`; rolls the session transaction back |

`select` through `relate` are conveniences. Each is translated into Atlantis Cypher
(for example, `relate` becomes `MATCH (a), (b) WHERE id(a) = $from AND id(b) = $to
CREATE (a)-[e:EDGE_TABLE $data]->(b) RETURN e`) and runs through the same planner,
permissions and transaction path as `query`. Record ids are passed in text form, such as
`"Person:alice"`, where a parameter is declared as a record id.

### Live Notifications

```json
{"id": 1, "method": "live", "params": ["LIVE MATCH (o:Order) WHERE o.status = 'open' RETURN o"]}
{"id": 1, "result": "01JC5Z8Q1N3X6W7Y8Z9A0B1C2D"}
```

Matching changes are pushed on the same socket without a request id:

```json
{"result": {"id": "01JC5Z8Q1N3X6W7Y8Z9A0B1C2D", "action": "CREATE", "record": {"id": "Order:1042", "status": "open", "total": 129.5}}}
{"result": {"id": "01JC5Z8Q1N3X6W7Y8Z9A0B1C2D", "action": "UPDATE", "record": {"id": "Order:1042", "status": "open", "total": 140.0}}}
{"result": {"id": "01JC5Z8Q1N3X6W7Y8Z9A0B1C2D", "action": "DELETE", "record": {"id": "Order:1042"}}}
```

`action` is `CREATE`, `UPDATE` or `DELETE` (or `KILLED`, see below). A record that stops matching the `WHERE`
clause is reported as `DELETE` for that subscriber, and a record that starts matching is
reported as `CREATE`. Notifications are emitted only after the writing transaction
commits; events, by contrast, run inside that transaction. Notifications are filtered by
the subscriber's permissions at delivery time.

Each live query has a bounded notification queue on the server. If a slow client lets
the queue overflow, the server kills that live query and sends a final notification with
`action` `KILLED`; the client catches up through `db.changes` and registers a new live
query. In Phase 4, a live query covers one table, has no aggregation, `ORDER BY` or
`LIMIT`, and sends no initial snapshot. Closing the socket, `kill`, `invalidate`, or session expiry ends every live
query of the session. Semantics are owned by
[14-live-queries-events.md](14-live-queries-events.md).

### Transactions over RPC

`begin` opens a transaction bound to the session. Subsequent `query` and convenience
calls run inside it until `commit` or `cancel`. A session holds at most one open
transaction, and an idle transaction is cancelled when the server's transaction timeout
expires. `BEGIN; ... COMMIT;` inside a single `query` call works without `begin`.
Conflict handling and isolation are in
[05-transactions-consistency.md](05-transactions-consistency.md).

## JSON and CBOR (Planned — Phase 5)

Both HTTP and RPC accept JSON or CBOR. The WebSocket format is chosen with the
`Sec-WebSocket-Protocol` subprotocol `json` or `cbor`; HTTP uses `Content-Type` and
`Accept`.

| `Value` type | JSON encoding | CBOR encoding |
|---|---|---|
| `Null`, `Bool`, `Integer`, `Float`, `String` | native | native |
| `Array`, `Object` | array, object | array, map (key order preserved) |
| `Vector` | array of numbers | typed array of `f32` |
| `Bytes` | base64 string | byte string |
| `Decimal` | string | decimal fraction |
| `Datetime` | RFC 3339 string | tagged epoch time with nanoseconds |
| `Duration` | ISO 8601 duration string | tagged value |
| `Uuid` | string | tagged UUID |
| `RecordId` | string, `"Person:alice"` | tagged value |
| `Geometry` | GeoJSON object | tagged GeoJSON-shaped map |
| `Range` | string | tagged value |

JSON is convenient for humans and `curl`, but it loses types: a datetime and a string
look the same on the way back. CBOR preserves every `Value` type exactly and is smaller,
so the SDKs in [19-client-sdks.md](19-client-sdks.md) use CBOR by default. The exact CBOR
tag numbers are fixed by the `atlantis-server` protocol module when Phase 5 ships.

## Bolt (Planned — Phase 6, optional)

Builds with the cargo feature `bolt` can serve the Neo4j Bolt protocol on port **7687**
when started with `--bolt`. The goal is to let existing Neo4j drivers and tools run
Cypher against Atlantis DB without a new SDK.

- Statements go through the same parser and engine as `query`. Atlantis-only extensions
  work, and Neo4j features that Atlantis Cypher does not support fail with a clear error.
- The Bolt database name selects namespace and database as `ns.db`, for example `acme.shop`.
- Authentication uses the Bolt `basic` scheme for system users and the `bearer` scheme
  for tokens from `/signin` or an external JWT issuer.
- Bolt explicit transactions map to session transactions.
- Values outside the Bolt type system (`Decimal`, `Geometry` other than points,
  `RecordId`, `Range`) are sent as strings.

Bolt is optional because it adds a large protocol surface for a compatibility benefit
only. HTTP and WebSocket RPC remain the primary protocols.

## TLS and CORS (Planned — Phase 5)

With `--tls-cert` and `--tls-key`, the main port serves HTTPS and WSS only, and the Bolt
port requires TLS too. Certificates are reloaded on `SIGHUP` so they can rotate without
a restart. Without TLS, terminate TLS at a reverse proxy and keep the server bound to a
private interface.

CORS is disabled by default. `--allow-origin https://app.example.com` lets browser
clients on that origin call the HTTP API and open `/rpc`. The server answers preflight
requests for the `Authorization`, `Atlantis-NS`, `Atlantis-DB` and `Content-Type`
headers.

## Sessions and `USE` (Planned — Phase 5)

A session holds:

| State | Set by |
|---|---|
| Namespace and database | `use` RPC, `USE acme.shop;`, `Atlantis-NS` / `Atlantis-DB` headers |
| Identity | `signin`, `signup`, `authenticate`, `Authorization` header |
| Session variables | `let`, `unset` |
| Live queries | `live`, `LIVE MATCH`, `kill` |
| Open transaction | `begin`, `commit`, `cancel` |

`USE acme.shop;` inside a query switches the namespace and database for the rest of that
query and, on a WebSocket session, for later requests. `USE acme;` changes only the
namespace. A `USE` that the identity may not access fails with `PermissionDenied` and
leaves the session unchanged. A record user or database user is pinned to the namespace
and database of its token, so `USE` cannot move it elsewhere.

The embedded `Engine` has an implicit session with namespace `atlantis` and database
`default` and full privileges, matching the embedded defaults in
[03-data-model.md](03-data-model.md).

## Import and Export (Planned — Phase 5)

Export writes a **Cypher script**: plain Atlantis Cypher that `import` replays. Using
the query language as the dump format means a dump is readable, diffable and editable,
and needs no separate loader.

```cypher
// Atlantis DB export — ns: acme, db: shop
USE acme.shop;

// Schema
CREATE TABLE Person SCHEMAFULL;
CREATE FIELD email ON Person TYPE string ASSERT value =~ '.+@.+';
CREATE TABLE WORKS_AT TYPE EDGE FROM Person TO Company;
CREATE INDEX person_email FOR (p:Person) ON (p.email);
CREATE ANALYZER english TOKENIZER unicode FILTERS [lowercase, ascii, snowball('english')];
CREATE FULLTEXT INDEX doc_text FOR (d:Doc) ON EACH [d.title, d.body] OPTIONS {analyzer: 'english'};

// Records
BEGIN;
CREATE (:Person {id: rid('Person', 'alice'), name: 'Alice', email: 'alice@example.com'});
CREATE (:Person {id: rid('Person', 'bob'), name: 'Bob', email: 'bob@example.com'});
COMMIT;
BEGIN;
MATCH (a:Person), (c:Company) WHERE id(a) = rid('Person', 'alice') AND id(c) = rid('Company', 'acme')
CREATE (a)-[:WORKS_AT {id: rid('WORKS_AT', '01JC5Z9B7K')}]->(c);
COMMIT;
```

The order is: namespace and database, analyzers, tables, fields, indexes and
constraints, access methods, events, then records in batched transactions, with node
tables before edge tables so that every edge's `in` and `out` exist. Data batches are
written as literal `CREATE` statements that preserve RecordIds. Events are created after
the data so that importing does not fire them.

What is not exported: system users (recreate them with `CREATE USER`), live queries,
change-feed history, and index contents (indexes are rebuilt on import). An export is a
consistent snapshot: it reads all records at one snapshot version.

## Backup and Restore (Planned — Phase 5)

Backups are **physical**: they copy KV data, not Cypher.

| | Export / import | Backup / restore |
|---|---|---|
| Scope | One database | Whole datastore (all namespaces, system users) |
| Format | Cypher script | Backend snapshot files |
| Index data | Rebuilt | Copied |
| Speed on large data | Slow | Fast |
| Runs through | Server session and permissions | Datastore, requires host access |

`atlantis backup rocksdb://data DIR` takes an online, consistent snapshot through the
RocksDB checkpoint mechanism while the server keeps serving. `atlantis restore DIR
rocksdb://target` writes a datastore that `atlantis start` can open. Restoring over a
running datastore is refused. For `tikv://` clusters, backup and restore use TiKV's own
tooling; see [20-distributed-architecture.md](20-distributed-architecture.md).

`atlantis upgrade` migrates a datastore's catalog and key encoding to the version the
binary expects. `atlantis start` refuses to open a datastore with an older storage
version and prints the `upgrade` command to run. Take a backup first.

## Metrics and Observability (Planned — Phase 5)

`GET /metrics` serves Prometheus text format. It requires authentication unless the
configuration file allows anonymous metrics on a private address. Metric families cover:

- requests by protocol, method and outcome, with latency histograms;
- open sessions, WebSocket connections and live queries;
- transactions committed, rolled back, and failed with `TxConflict`;
- storage backend latency and cache hit rates (record cache, adjacency cache, HNSW cache);
- background work: index builds, change-feed cleanup, retention.

Metric names use the `atlantis_` prefix. The authoritative list is the metrics registry
in `atlantis-server` once it exists, not this chapter.

Logs are structured (JSON when not attached to a terminal) and carry a request id that is
also returned in the `Atlantis-Request-Id` response header, so a client error can be
traced to a log line.

## Design Decisions

- **One port for HTTP and RPC.** Simplifies firewalls, TLS and proxies. Bolt keeps its
  own port because Neo4j drivers expect 7687.
- **Cypher as the RPC core.** Convenience methods compile to Cypher, so permissions,
  transactions and live queries have a single implementation.
- **CBOR by default in SDKs.** JSON cannot round-trip `Datetime`, `RecordId`, `Decimal`
  or `Bytes` without ambiguity.
- **Cypher script as export format.** Human-readable, versionable, and replayed by the
  same engine that serves queries.
- **Secure defaults.** Loopback bind, CORS off, authentication on.

## What's Next

- [18-security-auth.md](18-security-auth.md) — users, access methods, tokens and permissions
- [19-client-sdks.md](19-client-sdks.md) — SDKs built on this RPC surface
- [20-distributed-architecture.md](20-distributed-architecture.md) — running many `atlantis` nodes
- [22-configuration.md](22-configuration.md) — server configuration file
