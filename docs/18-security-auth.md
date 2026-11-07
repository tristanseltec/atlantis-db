# 18 — Security, Authentication and Permissions

> **Status:** Planned — Phase 5
> **Crates:** `atlantis-auth` (planned), `atlantis-engine`, `atlantis-server` (planned), `atlantis-cypher`

This chapter describes who can connect to Atlantis DB, how they prove it, what they may
read and write, and what is recorded about it. It covers system users and roles, access
methods for application users (`TYPE RECORD`) and external identity providers
(`TYPE JWT`), tokens and sessions, table and field permissions, password hashing,
audit logging, and the threat model behind the defaults.

## Implemented Today

Nothing in this chapter is implemented. The embedded engine has no users, no
authentication and no permission checks: whoever holds the `Engine` handle can run any
query, and the operating system's file permissions on the data directory are the only
protection (see `crates/atlantis-db/src/engine.rs`).

Everything below is the **target design** for Phase 5. The embedded engine keeps its
trusted, full-privilege behaviour after Phase 5; authentication applies to sessions
opened through `atlantis-server` (see [17-server-protocols.md](17-server-protocols.md)).

## Identity Model (Planned — Phase 5)

There are two kinds of identity:

| Identity | Defined by | Typical user | Bound to |
|---|---|---|---|
| **System user** | `CREATE USER` | Operators, developers, back-end services, analysts | ROOT, a namespace, or a database |
| **Access-method identity** | `CREATE ACCESS` | End users of an application, external IdP subjects | One database |

System users are stored in the catalog under `/sys/...` (ROOT level) or inside the
namespace or database they belong to. Access-method identities are ordinary records in
an application table (for `TYPE RECORD`) or claims inside a verified token (for
`TYPE JWT`).

## System Users and Roles (Planned — Phase 5)

```cypher
CREATE USER admin ON ROOT PASSWORD $pw ROLES [owner];

USE acme;
CREATE USER ops ON NAMESPACE PASSWORD $pw ROLES [editor];

USE acme.shop;
CREATE USER analyst ON DATABASE PASSWORD $pw ROLES [viewer];
```

`ON NAMESPACE` and `ON DATABASE` refer to the session's current namespace and database,
so the target is chosen with `USE` first. A user's level is a scope: a namespace user
reaches every database in that namespace and nothing outside it.

Users are changed and removed with the same pattern:

```cypher
ALTER USER analyst ON DATABASE PASSWORD $newPw;
ALTER USER analyst ON DATABASE ROLES [editor];
DROP USER analyst ON DATABASE;
```

### Roles

| Role | Data | Schema (tables, fields, indexes, events, access methods) | Users |
|---|---|---|---|
| `owner` | read and write | create, alter, drop | manage users at its own level and below |
| `editor` | read and write | create, alter, drop | none |
| `viewer` | read | read definitions only | none |

Roles apply at the user's level and every level below it. An `owner` on ROOT can create
namespaces and root users; an `owner` on a database can manage users of that database
only. No role can grant a privilege it does not hold, so an `editor` cannot create an
`owner`.

System users are not subject to table `PERMISSIONS` clauses. Those clauses govern
access-method identities; system users are governed by their roles. This keeps
operational access predictable and keeps application rules focused on end users.

### The First Root User

A new server datastore has no users. `atlantis start --user root --pass "$PW"` creates
that root user with role `owner` on first start if no root user exists, and is ignored
afterwards. A server with no root user and no `--user` flag refuses to start unless it
is bound to loopback with `--insecure-no-auth`.

## Access Methods (Planned — Phase 5)

An access method defines how non-system identities authenticate to one database. It is
named and versioned with the schema, and it is exported with the database (see
[17-server-protocols.md](17-server-protocols.md)).

### `TYPE RECORD`: application sign-up and sign-in

```cypher
CREATE ACCESS account ON DATABASE TYPE RECORD
  SIGNUP { CREATE (u:User {email: $email, pass: crypto.argon2.generate($pass)}) RETURN u }
  SIGNIN { MATCH (u:User {email: $email}) WHERE crypto.argon2.compare(u.pass, $pass) RETURN u }
  DURATION FOR TOKEN 15m, FOR SESSION 24h;
```

- `SIGNUP` and `SIGNIN` are Cypher blocks. The fields of the client's sign-in request
  (other than `ns`, `db` and `access`) are bound as parameters, here `$email` and `$pass`.
- A block must return exactly one record. That record becomes the session's `$auth`.
  Zero rows means the attempt failed; the client always sees the same generic
  "authentication failed" error whether the user does not exist or the password is wrong.
- The blocks run with the privileges of the access method, not of the anonymous caller,
  so `SIGNUP` can create a `User` record even when anonymous callers cannot.
- Uniqueness of the identifying field is the schema's job:
  `CREATE CONSTRAINT user_email_unique FOR (u:User) REQUIRE u.email IS UNIQUE;`

Client flow over HTTP:

```bash
curl -X POST http://localhost:7700/signup -H "Content-Type: application/json" \
  -d '{"ns": "acme", "db": "shop", "access": "account", "email": "an@example.com", "pass": "..."}'
# → {"token": "eyJ..."}
```

### `TYPE JWT`: external identity providers

```cypher
CREATE ACCESS partner ON DATABASE TYPE JWT
  URL 'https://login.example.com/.well-known/jwks.json'
  DURATION FOR SESSION 12h;

CREATE ACCESS internal ON DATABASE TYPE JWT
  ALGORITHM HS512 KEY $sharedSecret
  DURATION FOR SESSION 1h;
```

A `TYPE JWT` method accepts tokens issued elsewhere. With `URL`, Atlantis DB fetches the
issuer's JWKS document, caches the keys, and refreshes the cache when a token arrives
with an unknown `kid`. With `ALGORITHM ... KEY`, tokens are verified with a fixed key;
asymmetric algorithms take a public key, so Atlantis DB never holds the issuer's signing
key.

Verification checks the signature, `exp`, `nbf`, and the `ns`, `db` and `ac` (access
method name) claims, which must name this database and this method. The verified claims
become `$token`. If the token carries an `id` claim holding a RecordId, that record is
loaded as `$auth`; otherwise `$auth` is `NULL` and permissions rely on `$token` claims.

The client sends the external token directly, with `Authorization: Bearer <jwt>` over
HTTP or the `authenticate` RPC method. There is no sign-in call.

## Tokens and Sessions (Planned — Phase 5)

| Concept | Meaning | Set by |
|---|---|---|
| Token duration | How long a token issued by `/signin` or `/signup` is accepted for starting a session | `DURATION FOR TOKEN` |
| Session duration | How long a session, once authenticated, stays authenticated | `DURATION FOR SESSION` |

A short token and a longer session is the intended combination: a leaked token is only
useful for a few minutes, while an open WebSocket session keeps working. When the session
duration expires, the server ends its live queries, cancels its open transaction, and
rejects further requests until the client authenticates again.

System users receive tokens with server-configured durations (see
[22-configuration.md](22-configuration.md)). Tokens issued by Atlantis DB are JWTs
signed with a per-datastore key stored under `/sys/...`. Rotating that key invalidates
all outstanding Atlantis-issued tokens. The `invalidate` RPC method ends a session
immediately; a token itself cannot be revoked before it expires, which is why token
durations are short.

### Session Parameters

These parameters are available in every statement, in `PERMISSIONS` clauses, in events
and in live-query filters:

| Parameter | Content |
|---|---|
| `$auth` | The authenticated record (`TYPE RECORD`, or `TYPE JWT` with an `id` claim), or a description of the system user; `NULL` when anonymous |
| `$session` | Object with `ns`, `db`, `ac`, `id` (session id), `ip`, `origin`, `exp` |
| `$access` | Name of the access method used, or `NULL` for system users |
| `$token` | Verified token claims as an object |

They are read-only. `LET` / the `let` RPC method cannot shadow them.

## Table and Field Permissions (Planned — Phase 5)

Permissions decide what an access-method identity may do with each record.

```cypher
ALTER TABLE Post PERMISSIONS
  FOR read WHERE p.published OR p.author = $auth.id
  FOR create, update, delete WHERE p.author = $auth.id;
```

The actions are `read`, `create`, `update` and `delete`. Each `WHERE` is an Atlantis
Cypher predicate over the record being checked, written through a single free variable
(`p` above), plus session parameters. `FULL` allows an action unconditionally and `NONE`
denies it:

```cypher
ALTER TABLE Product PERMISSIONS FOR read FULL FOR create, update, delete NONE;
```

The grammar of `PERMISSIONS` clauses on `CREATE TABLE` and `ALTER TABLE` belongs to
[07-schema-ddl.md](07-schema-ddl.md).

### Secure Default

A table with no `PERMISSIONS` clause is `NONE` for every action for access-method
identities. A new table is therefore invisible to application users until its owner
writes rules. System users are unaffected.

### Field Permissions

Fields can narrow access further:

```cypher
CREATE FIELD email ON User TYPE string
  PERMISSIONS FOR read WHERE u.id = $auth.id FOR update WHERE u.id = $auth.id;
CREATE FIELD role ON User TYPE string DEFAULT 'member'
  PERMISSIONS FOR read FULL FOR create, update NONE;
```

A field whose `read` permission fails is omitted from the returned record, as if absent;
the rest of the record is still returned. A write that sets a field whose `create` or
`update` permission fails is rejected with `PermissionDenied`. A `NONE` field can still
receive its `DEFAULT`, which is how `role` above is always `'member'` for self-sign-ups.

### Edge Tables

Edge tables are tables, so they carry their own permissions. Traversing
`(a)-[:WORKS_AT]->(c)` requires `read` on `WORKS_AT` and on the tables of both
endpoints. Creating an edge requires `create` on the edge table; the endpoints only need
to be readable.

## How Permissions Are Enforced (Planned — Phase 5)

Permissions are enforced in `atlantis-engine`, below every protocol, so HTTP, RPC, Bolt
and SDK convenience methods behave the same way.

**Reads are rewritten, not post-filtered.** When the planner (see
[15-query-planner-executor.md](15-query-planner-executor.md)) binds a pattern variable to
a table for an access-method identity, it adds that table's `read` predicate to the
pattern's filter, with `p` replaced by the pattern variable:

```cypher
// The application user writes:
MATCH (x:Post) WHERE x.tags CONTAINS 'rust' RETURN x.title
// The engine plans:
MATCH (x:Post) WHERE x.tags CONTAINS 'rust'
                 AND (x.published OR x.author = $auth.id) RETURN x.title
```

Because the predicate is part of the plan, the optimizer can use an index on
`Post.author`, aggregates such as `count(*)` see only permitted rows, and `LIMIT` returns
the expected number of permitted rows. Invisible records are skipped silently; a read
never fails because some records are hidden.

The same injection applies to search procedures: `db.vector.search`, `db.text.search`
and `db.search.hybrid` filter candidates by the `read` predicate before ranking is
finalized, so results neither leak hidden records nor return fewer than `k` permitted
hits when more exist (see [10-vector-index.md](10-vector-index.md)).

**Writes are checked per record.**

| Action | Checked against | Failure |
|---|---|---|
| `create` | The new record after defaults and computed fields | `PermissionDenied`; the statement fails |
| `update` | Must be readable before; `update` predicate on the before-image and on the after-image | Unreadable records are not matched; failing after-image → `PermissionDenied` |
| `delete` | Must be readable; `delete` predicate on the before-image | Unreadable records are not matched; otherwise `PermissionDenied` |

Checking the after-image prevents a user from updating a record into a state they could
not have created, such as reassigning `author` to someone else.

**Live queries** are registered with the subscriber's identity. For every change, the
dispatcher evaluates the `read` predicate against the new record version for that
subscriber before sending a notification, and field permissions remove hidden fields from
`record`. A change that makes a record unreadable is delivered as `DELETE`. When the
session expires or is invalidated, its live queries are killed. Details are in
[14-live-queries-events.md](14-live-queries-events.md).

**Events** run synchronously inside the triggering transaction, so an event failure
aborts the user's write. They run with the privileges of the table owner, not of the user
whose write triggered them, so an event can maintain an audit table that users cannot
write. Live notifications and change-feed entries are produced only after commit.

**Change feeds** (`db.changes`) are filtered the same way as live notifications for
access-method identities: each change is checked against the table's `read` predicate
for the caller, and hidden fields are removed. This matters because a client whose live
query was killed on notification-queue overflow catches up through `db.changes`. System
users see unfiltered changes.

## Password Hashing (Planned — Phase 5)

All stored passwords use **Argon2id** through `atlantis-auth`:

- `PASSWORD $pw` on system users is hashed before it reaches storage. The plaintext is
  never written to KV, logs, change feeds or exports.
- `crypto.argon2.generate(text)` returns a PHC-format hash string that includes its
  parameters and salt; `crypto.argon2.compare(hash, text)` verifies in constant time.
- Default parameters follow current OWASP guidance for Argon2id (memory-hard, single
  lane) and are configurable server-wide. Because the parameters are stored in each hash,
  raising them later does not break existing hashes; hashes are upgraded on the next
  successful sign-in.

`crypto.argon2.*` is the only supported way to store passwords in application tables.

## Audit Logging (Planned — Phase 5)

When `--audit-log <path>` is set (or configured in the configuration file), the server
writes one JSON line per security-relevant event:

- sign-in and sign-up success and failure, with access method, level and client IP;
- token authentication failures (bad signature, expired, wrong `ns`/`db`/`ac`);
- user, role, access-method and permission DDL;
- `PermissionDenied` on writes;
- `USE` attempts outside the identity's scope;
- server start, shutdown, backup, restore and upgrade.

Entries carry the time, session id, request id, identity (user name or `$auth` id), and
the statement kind. They never carry passwords, tokens, or parameter values, because
parameters routinely hold secrets and personal data. Query text is included only when
audit verbosity is raised explicitly. The audit log is append-only from the server's
point of view; shipping and retaining it is the operator's responsibility.

## Threat Model (Planned — Phase 5)

| Threat | Mitigation |
|---|---|
| Unauthenticated network access | Authentication required; loopback bind by default; `--insecure-no-auth` refused on non-loopback addresses |
| Credential stuffing and brute force | Argon2id cost; per-IP and per-identity sign-in rate limiting with backoff; generic failure messages |
| Stolen database files | Hashes, not passwords; disk encryption is delegated to the host or storage layer |
| Stolen token | Short token duration; session duration bounds; signing-key rotation |
| Forged external token | Signature verification against JWKS or a configured key; `ns`, `db`, `ac`, `exp` checks; `none` algorithm rejected |
| Horizontal privilege escalation (user A reads user B's data) | Read predicates injected into plans; after-image checks on writes; secure `NONE` default |
| Privilege escalation through roles | Roles cannot grant above themselves; access-method blocks run with fixed, method-scoped privileges |
| Leak through search or live queries | Permission predicates applied inside search procedures and per live-query notification |
| Cypher injection | Parameters are values, never spliced into query text; SDKs pass `params` separately |
| Resource exhaustion | Query timeouts, request body limits, per-session live-query limits (see [22-configuration.md](22-configuration.md)) |
| Eavesdropping | TLS via `--tls-cert`/`--tls-key` or a terminating proxy; warning when bound publicly without TLS |
| Cross-origin abuse from browsers | CORS off by default; explicit `--allow-origin` list |

Out of scope: a compromised host or operating-system account, side channels on shared
hardware, and the security of external identity providers.

### Secure Defaults

- Authentication on; the server will not run without a root user unless explicitly
  started insecure on loopback.
- Bind to `127.0.0.1:7700`; CORS disabled.
- Tables are `NONE` for access-method identities until permissions are written.
- Change feeds filtered by `read` permissions for access-method identities.
- No secret ever appears in logs, audit entries, exports or error messages.

## Design Decisions

- **Two identity kinds.** System users with fixed roles keep operations simple; access
  methods let each application define its own sign-up and sign-in in Cypher without a
  separate auth service.
- **Predicate injection instead of post-filtering.** It keeps aggregates, `LIMIT` and
  search results correct and lets indexes serve permission predicates.
- **Permissions in the engine.** Every protocol and SDK goes through one enforcement
  point.
- **Embedded stays trusted.** An in-process library cannot defend against its own host
  process, so pretending otherwise would add cost without security.

## What's Next

- [17-server-protocols.md](17-server-protocols.md) — `/signin`, `/signup`, RPC `authenticate`
- [19-client-sdks.md](19-client-sdks.md) — authentication from each SDK
- [07-schema-ddl.md](07-schema-ddl.md) — full `PERMISSIONS` and `CREATE ACCESS` grammar
- [14-live-queries-events.md](14-live-queries-events.md) — live-query and event semantics
