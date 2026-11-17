# Deprecated: Python servers

:::{warning}
**The Python servers are fully deprecated.** They are not tested in CI, not
published to PyPI, and receive no new features or fixes. This page is kept as
a record of how they were run. For anything new use the two Rust servers,
`secantusd-mdb` (MongoDB) and `secantusd-pg` (PostgreSQL): see
[Installation](installation.md) and [Quickstart](quickstart.md).

The `SecantusDB` package on PyPI is **not** these servers: it is a launcher
and prebuilt binaries of the Rust servers, imported as `secantusdb`
(`from secantusdb import MongoServer, PostgresServer`). The Python servers
below (`secantus.SecantusDBServer`, `secantusd-py`, `secantusd-py-pg`) exist
only in a source checkout of the repository.
:::


SecantusDB runs in two modes from the same `SecantusDBServer` class:

- **Embedded** — the server runs on a daemon thread inside your Python
  process. Best when your application owns the database lifetime.
- **Standalone daemon** — a long-running process that other tools,
  processes, or language runtimes connect to over TCP. **This is the
  drop-in `mongod` replacement**: any standard MongoDB driver or
  command-line tool (`pymongo`, `mongo-go-driver`, `mongosh`,
  `mongodump`, `mongorestore`, ...) connects unchanged — just point
  the URI at SecantusDB's host:port.

Both modes default to **on-disk storage** at `./secantus-data` so data
survives restarts. Pass `storage_path=":memory:"` (or `--storage-path
:memory:` on the CLI) for a temporary, throwaway-data mode.

## Embedded (in-process)

A real script — start the server, connect, do work, shut down. The
context manager handles startup and shutdown; `./secantus-data` is
created on first run and reopened intact on later runs.

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=27017) as server:
    client = MongoClient(server.uri)
    bottles = client["wine_cellar"]["bottles"]

    bottles.insert_one({"_id": 1, "name": "Pommard 2018", "year": 2018})
    bottles.insert_many(
        [
            {"_id": 2, "name": "Brunello 2015", "year": 2015},
            {"_id": 3, "name": "Barolo 2017", "year": 2017},
        ]
    )

    bottles.create_index([("year", 1)])
    older = list(bottles.find({"year": {"$lte": 2017}}).sort("year"))
    print([b["name"] for b in older])
    # ['Brunello 2015', 'Barolo 2017']
```

When the `with` block exits the server stops cleanly. `./secantus-data`
keeps the inserted documents — re-run the same script and `bottles.find`
sees them again.

For a different on-disk location, pass `storage_path=`:

```python
with SecantusDBServer(port=27017, storage_path="/var/lib/cellar") as server:
    ...
```

## Standalone daemon

Long-running process; other tools, processes, or language runtimes
connect to it over TCP. Two ways to launch.

### CLI

```bash
secantusd-py --host 127.0.0.1 --port 27017
# storage at ./secantus-data by default
```

`secantusd-py` is the standalone single-node Python server, available from a
source checkout of the repository (it is no longer published). The module form
`python -m secantus` invokes the same entry point.

Then from another shell — same commands you'd run against `mongod`:

```bash
mongosh mongodb://127.0.0.1:27017
mongodump   --uri mongodb://127.0.0.1:27017 --out ./dump
mongorestore --uri mongodb://127.0.0.1:27017 ./dump
```

Or from a Python script connecting to the running daemon:

```python
from pymongo import MongoClient

client = MongoClient("mongodb://127.0.0.1:27017")
client["wine_cellar"]["bottles"].insert_one({"_id": 1, "name": "Pommard 2018"})
```

Or from Go — `mongo-go-driver` works the same way (see the
[Go-driver validation report](validation-report-go.md)):

```go
client, _ := mongo.Connect(options.Client().ApplyURI("mongodb://127.0.0.1:27017"))
```

CLI flags:

| Flag | Default | Notes |
|---|---|---|
| `--host` | `127.0.0.1` | bind address |
| `--port` | `27017` | TCP port (matches MongoDB's standard so existing tools just work) |
| `--storage-path` | `./secantus-data` | WiredTiger home; pass `:memory:` for ephemeral |
| `--log-level` | `INFO` | DEBUG / INFO / WARNING / ERROR |

`SIGINT` and `SIGTERM` are handled cleanly — the daemon calls
`server.stop()` in the signal handler so WiredTiger shuts down without
leaving stale lock files.

### Programmatic

When you embed the daemon in a larger long-running app (process
supervisor, sandbox harness, etc.):

```python
from secantus import SecantusDBServer

server = SecantusDBServer(host="127.0.0.1", port=27017, storage_path="/var/lib/cellar")
server.start()       # returns once the listener is bound
server.wait()        # blocks until server.stop() from another thread / signal
```

## Temporary (in-memory) mode

For scratch experiments or tests, set `storage_path=":memory:"`. The
server uses a tempdir WiredTiger home that's wiped on `stop()` — no
files left behind, no port conflicts, fully parallel-safe. Combine with
`port=0` so the OS picks a free port:

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0, storage_path=":memory:") as server:
    client = MongoClient(server.uri)
    # ... use client; nothing persists past this block ...
```

Pytest fixture form:

```python
import pytest
from pymongo import MongoClient
from secantus import SecantusDBServer

@pytest.fixture
def client():
    with SecantusDBServer(port=0, storage_path=":memory:") as server:
        yield MongoClient(server.uri)
```

## Picking between them

| Use case | Mode |
|---|---|
| Application owns the DB lifetime | Embedded, on-disk default |
| Local dev replacement for `mongod` | Daemon (CLI), on-disk default |
| CI scratch DB shared across processes | Daemon (CLI), persistent path |
| Multi-language test (Python + Node + ...) | Daemon (CLI), fixed port |
| Inside a parent application | Programmatic daemon, `start()` + `wait()` |
| Pytest fixture / unit tests | Embedded, `port=0`, `:memory:` |

## Async-friendly

SecantusDB doesn't impose a threading model on client code — the server
runs each connection on its own daemon thread under the hood, so
`pymongo` clients (sync or via `motor`) just work.

## Cleanup

Always use the context-manager form for embedded mode, or call
`server.stop()` in a teardown / signal handler for daemon mode. The CLI
handles this for you. Letting the process exit without stopping the
server is fine for the `:memory:` mode (everything is daemon threads +
temp directories) but a persistent on-disk run will leave WiredTiger
lock files behind that recovery has to clear on next open.

## See also (Python-server pages)

- [Examples](examples.md) — connect, insert, index, query, drop.
- [Architecture](architecture.md) — what's running under the hood.
- [Indexes](indexes.md) — index acceleration semantics, `explain`, hints.
- [Aggregation](aggregation.md) — what pipeline stages and expression
  operators are supported.
- [Compatibility](compatibility.md) — known differences from real `mongod`.

## The Python SQL / PostgreSQL server

`SecantusPGServer` mirrors `SecantusDBServer`: construct, `start()`, `stop()`,
and a context-manager form. `port=0` picks a free port (handy in tests).

```python
from secantus.sql import SecantusPGServer

with SecantusPGServer(port=5432, storage_path="./secantus-data") as server:
    print(server.uri)          # postgresql://127.0.0.1:5432/postgres
    ...                        # connect and query; the server stops on exit
```

For a long-running daemon, call `start()` and keep the process alive yourself:

```python
server = SecantusPGServer(port=5432, storage_path="./secantus-data")
server.start()
# ... the accept loop runs on a daemon thread; block your main thread here ...
server.stop()
```

Or run it straight from the command line with `secantusd-py-pg` (from a source
checkout; the SQL sibling of the Mongo `secantusd-py` daemon):

```console
$ secantusd-py-pg --host 127.0.0.1 --port 5432 --storage-path ./secantus-data
```

The connection's **database** selects the SecantusDB storage database; a SQL
**table** is a collection; a **row** is a document.

### Both protocols, one dataset

Point a `SecantusPGServer` at the *same* `Storage` a `SecantusDBServer` owns and
the two protocols serve the same data live:

```python
from pymongo import MongoClient
from secantus import SecantusDBServer
from secantus.sql import SecantusPGServer

mongo = SecantusDBServer(port=27017)
sql = SecantusPGServer(port=5432, storage=mongo.storage)  # share the store
mongo.start()
sql.start()

# Write through MongoDB...
MongoClient(mongo.uri)["shop"]["products"].insert_one(
    {"_id": 1, "name": "Widget", "price": 9.99}
)
# ...read through SQL (database "shop" -> the same storage db).
```


### The Python SQL server: authentication, roles and privileges

By default the server trusts every connection (matching the Mongo server's
`require_auth=False` default). Turn on **SCRAM-SHA-256** by supplying users:

```python
server = SecantusPGServer(port=5432, require_auth=True, users={"alice": "s3cret"})
```

```python
pg8000.dbapi.connect(user="alice", password="s3cret", host="127.0.0.1", port=5432, database="db")
```

**TLS** is enabled by passing a certificate and key; the server answers the
client's `SSLRequest` and wraps the socket:

```python
server = SecantusPGServer(
    port=5432,
    tls_cert_file="server.pem",
    tls_key_file="server.key",
)
```

```python
import ssl
ctx = ssl.create_default_context(cafile="ca.pem")
pg8000.dbapi.connect(user="alice", host="127.0.0.1", port=5432, database="db", ssl_context=ctx)
```

#### Roles (`CREATE ROLE` / `CREATE USER`)

SQL-level roles are recorded in the catalog and surfaced through `pg_catalog.pg_roles`,
so `psql`'s `\du` and role-aware tooling see them:

```sql
CREATE ROLE analyst;
CREATE USER app WITH PASSWORD 'secret' CREATEDB;   -- USER implies LOGIN
ALTER ROLE analyst WITH LOGIN;
GRANT SELECT ON orders TO analyst;                 -- recorded + enforced (see below)
DROP ROLE analyst;
```

`CREATE ROLE` / `CREATE USER` (with `LOGIN` / `SUPERUSER` / `CREATEDB` / `CREATEROLE` /
`INHERIT` / `REPLICATION` and their `NO…` negations, `PASSWORD`, `CONNECTION LIMIT`),
`ALTER ROLE`, and `DROP ROLE` are stored and reflected. Role-membership `GRANT` /
`REVOKE` (`GRANT admin TO alice`) is accepted but not enforced. Table-privilege
`GRANT` / `REVOKE` (`GRANT SELECT ON t TO alice`) **is** recorded and enforced — see
[Table privileges](#table-privileges-grant--revoke) below. The connecting user
always appears in `pg_roles` as a superuser login role, like Postgres' bootstrap
superuser.

These SQL roles are a schema-shape / reflection record, **distinct from the wire
server's SCRAM auth users** (the `users={...}` constructor argument above): creating
a SQL role does not by itself add a login credential, and vice versa.

#### Authorization (RBAC)

By default the SQL surface is **unrestricted**: in trust mode (no `require_auth`)
and for the embedded `run_sql` API, any statement runs against any database. To
gate statements, start the server with `require_auth=True` **and** per-user role
bindings via `user_roles`. Authorization then reuses the **same role model as the
Mongo server** (`secantus.rbac`), so a SQL client and a Mongo client on the same
`Storage` are held to the same roles:

```python
server = SecantusPGServer(
    port=5432,
    require_auth=True,
    users={"analyst": "s3cret", "editor": "hunter2"},
    user_roles={
        "analyst": [{"role": "read", "db": "shop"}],       # SELECT only
        "editor":  [{"role": "readWrite", "db": "shop"}],  # + INSERT/UPDATE/DELETE/DDL
    },
)
```

Each statement maps to one RBAC action on the connection's database and is
checked with `rbac.check_privilege`: reads (`SELECT`, `DECLARE … CURSOR`, and a
plain `WITH`) need `find`; `INSERT` / `UPDATE` / `DELETE` / `MERGE` (and any
data-modifying CTE) need the matching write action; DDL needs
`createCollection` / `dropCollection` / `createIndex`; `CREATE/DROP/ALTER ROLE`
and `GRANT` / `REVOKE` need user-admin actions. Transaction control
(`BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT`), `SET` / `SHOW`, cursor
navigation (`FETCH` / `MOVE` / `CLOSE`), and session-info queries need no
privilege. A denied statement returns SQLSTATE **`42501`**
(`insufficient_privilege`) and the connection survives.

Built-in roles (`read`, `readWrite`, `dbAdmin`, `dbOwner`, `root`, the
`*AnyDatabase` variants) resolve directly; custom roles resolve through the
shared roles table (`Storage.get_role` / the Mongo `createRole` command), so a
role defined once governs both protocols. An authenticated user with no role
binding can connect and run session-only statements but touches no data.

Without `user_roles`, authorization stays off — pass it only when you want the
per-database gate.

#### Table privileges (`GRANT` / `REVOKE`)

`GRANT`/`REVOKE` of `SELECT` / `INSERT` / `UPDATE` / `DELETE` (or `ALL`) on a table
are persisted and enforced as an **additive** layer over the Mongo RBAC roles
above:

```sql
GRANT SELECT, INSERT ON orders TO analyst;   -- analyst can now read + insert orders
GRANT SELECT ON orders TO PUBLIC;            -- everyone can read orders
REVOKE INSERT ON orders FROM analyst;        -- take the insert back
```

A data operation is authorized when the connection's Mongo role covers it **or**
a table grant does — so a grant *extends* access to a user whose role wouldn't
otherwise allow the operation (including a user with no role at all, or via a
`PUBLIC` grant), and a `REVOKE` takes that granted access back. Because the two
axes are additive, a table grant never *shrinks* what a broader Mongo role
(e.g. `readWrite`) already permits; use the role bindings for that. Enforcement
follows the same opt-in as the RBAC gate: with authorization off (trust mode /
embedded `run_sql`) grants are recorded and reflected but not enforced.

Grants surface through `information_schema.role_table_grants` /
`information_schema.table_privileges` and the `has_table_privilege([user,] table,
privilege)` function:

```sql
SELECT grantee, privilege_type FROM information_schema.role_table_grants;
SELECT has_table_privilege('analyst', 'orders', 'SELECT');   -- t
```

`ALL` expands to Postgres' seven table privileges for reflection fidelity, but
only `SELECT` / `INSERT` / `UPDATE` / `DELETE` are enforced (the other operations
don't exist in SecantusDB). Role-membership grants and grants on schemas /
databases / sequences remain accepted no-ops.

Grants can also be **column-scoped** for a finer grain than the whole table:

```sql
GRANT SELECT (id, title) ON articles TO editor;  -- editor reads only these columns
GRANT UPDATE (title) ON articles TO editor;      -- and updates only this one
```

A column grant authorizes exactly the named columns. A statement is allowed on
the column-grant path only when **every** column it touches is granted — a
`SELECT` of an ungranted column (including a `SELECT *`, or a column referenced
only in the `WHERE`) or an `UPDATE`/`INSERT` of an ungranted column is denied with
`42501`. A whole-table grant (or a broader Mongo role) still covers every column,
so column grants only *add* access — they never shrink it. Column grants surface
through `information_schema.column_privileges` and
`has_column_privilege([user,] table, column, privilege)`.

#### Switching identity (`SET ROLE` / `SET SESSION AUTHORIZATION`)

A session can change its **current role** with `SET ROLE`, leaving the **session
user** (the login identity) unchanged, and change both with `SET SESSION
AUTHORIZATION`:

```sql
SET ROLE analyst;                 -- current_user is now analyst; session_user unchanged
SELECT current_user, session_user;  -- ('analyst', 'joe')
RESET ROLE;                       -- back to the login role (SET ROLE NONE is equivalent)

SET SESSION AUTHORIZATION alice;  -- both current_user and session_user become alice
RESET SESSION AUTHORIZATION;      -- back to the login identity
```

The current role (`current_user` / `current_role` / `user`) is what the table
grants above are matched against, so a session that holds the `analyst` role can
`SET ROLE analyst` to pick up whatever was granted to `analyst`. `session_user`
always reports the login identity. `SHOW role` / `current_setting('role')` report
the current role (`none` when it tracks the session user).

When authorization is active, a session may only assume an identity it already
holds — its own login, a role in its bindings, or anything if it is a superuser
(`root`); assuming another identity to borrow its grants is denied with SQLSTATE
`42501`. In trust mode (and the embedded API) the switch is unrestricted. Role
*membership* is not otherwise tracked, so `SET ROLE` doesn't consult a membership
graph beyond the session's own role bindings.

#### Row-level security (`CREATE POLICY`)

Enable RLS on a table and attach policies to restrict which rows a role can see
or write:

```sql
ALTER TABLE doc ENABLE ROW LEVEL SECURITY;
CREATE POLICY owner_rows ON doc
    FOR ALL TO public
    USING (owner = current_user)         -- rows this role may read / target
    WITH CHECK (owner = current_user);   -- rows this role may write
```

A policy's `USING` predicate is AND'd into the `WHERE` of a `SELECT` / `UPDATE` /
`DELETE`, so only permitted rows are returned or affected; its `WITH CHECK`
predicate is validated against each row an `INSERT` / `UPDATE` writes (a violation
raises SQLSTATE `42501`). Identity functions (`current_user` / `session_user`) in
a policy resolve to the session's identity — combined with `SET ROLE`, one
connection can switch which rows it sees. `AS RESTRICTIVE` policies are AND'd with
the OR of the permissive ones; a `FOR` clause scopes a policy to one command and
`TO` scopes it to roles (default `public`). With RLS enabled and no applicable
permissive policy, the table is default-deny (no rows).

Enforcement follows the same opt-in as the rest of the RBAC surface: it bites
only when authorization is active and a superuser (`root`) bypasses it, so trust
mode and the embedded `run_sql` API record policies (visible in
`pg_catalog.pg_policies`) but don't enforce them. **Limitations:** `USING`
injection covers single-table `SELECT` / `UPDATE` / `DELETE` (a join doesn't get
the base table's policy applied); there's no table-owner concept (RLS applies to
every non-`root` role under active authorization, not "everyone except the
owner"); and `FORCE ROW LEVEL SECURITY` is recorded but behaves like `ENABLE`.

## Architecture of the Python server (historical)

:::{note}
This section describes the **deprecated Python server**, which first built
the layer map and the WiredTiger table layout. The Rust servers' architecture
is on [Architecture](architecture.md). The Rust MongoDB server keeps the same
document model (opaque BSON blobs, byte-sortable `_id` keys, typed sort-key
index columns), but the code below is Python-server code and the performance
and concurrency statements are about that server only.
:::

SecantusDB is layered roughly outermost-in: a TCP accept loop on top of a
wire-protocol codec, on top of a command dispatch table, on top of pure
operator engines (query / update / projection / aggregation / expressions),
on top of a WiredTiger-backed storage layer.

### Layers

#### `src/secantus/server.py` — `SecantusDBServer`

TCP accept loop on a daemon thread, one daemon thread per connection.
Owns the `Storage` and the `CursorRegistry`. Per-request, builds a fresh
`CommandContext(storage, cursors, db_name)` and calls into the command
dispatcher.

`port=0` lets the OS pick a free port — read it back from `server.port`
or `server.uri` after `start()`.

#### `src/secantus/wire.py`

The MongoDB wire codec:

- 16-byte header (little-endian).
- `OP_MSG` (op-code 2013) parse / build — the modern op-code that
  `pymongo` 4.x uses for everything after the handshake.
- Legacy `OP_QUERY` (2004) parse + `OP_REPLY` (1) build for the initial
  handshake `pymongo` does on connect.

`OP_MSG` kind-1 document sequences are merged into the body before
dispatch, so command handlers see a single flat document.

#### `src/secantus/commands.py`

Single dispatch table keyed on the first key of the request document.
Handles handshake (`hello` / `isMaster` / `ping` / `buildInfo` / ...) and
CRUD (`insert` / `find` / `update` / `delete` / `count` / `drop` /
`aggregate` / `findAndModify` / `listCollections` / ...).

Errors raised by handlers are caught and turned into
`{ok: 0, errmsg, code, codeName}`. Unknown commands return
`code: 59 CommandNotFound` so the connection survives.

#### `src/secantus/query.py` — `matches(doc, filter, vars=None)`

Pure document-vs-filter predicate. Field-level operators: `$eq`, `$ne`,
`$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$nin`, `$exists`, `$not`,
`$regex` + `$options`, `$type`, `$size`, `$all`, `$mod`, `$elemMatch`.
Document-level operators: `$and`, `$or`, `$nor`, `$expr`, `$jsonSchema`,
`$comment` (no-op). Dotted paths walk into both maps and arrays.

#### `src/secantus/projection.py` — `apply_projection(doc, spec)`

`find()`'s projection argument: inclusion / exclusion modes, `_id`
defaults, dotted paths, plus the `$elemMatch` projection operator that
returns the first array element matching a sub-filter.

#### `src/secantus/update.py` — `apply_update(doc, update)`

Operators: `$set`, `$unset`, `$inc`, `$mul`, `$min`, `$max`, `$push`,
`$pull`, `$addToSet`, `$pop`, `$rename`. Replacement-style updates
preserve `_id`. Mixing operators with replacement fields is rejected.

#### `src/secantus/expressions.py` — `evaluate(expr, doc, vars=None)`

The aggregation expression language: field paths (`"$x.y"`), `$$varname`
user vars + `$$ROOT` / `$$CURRENT`, `$literal`, arithmetic, comparison,
logical, `$cond` / `$ifNull`, `$size`, dates (with timezone support),
strings, arrays, conversions, `$mergeObjects`, ... see
[Aggregation](aggregation.md) for the full operator list.

#### `src/secantus/aggregate.py` — `apply_pipeline(docs, pipeline, ctx)`

Pipeline stages — `$match`, `$count`, `$limit`, `$skip`, `$sort`,
`$project`, `$addFields` / `$set`, `$unset`, `$unwind`, `$densify`,
`$replaceRoot` / `$replaceWith`, `$group`, `$lookup` (hash-join),
`$sample`, `$sortByCount`, `$facet`, `$bucket`, `$merge`, `$out`. See
[Aggregation](aggregation.md) for details.

#### `src/secantus/cursors.py` — `CursorRegistry`

Per-server, thread-safe map of int64 cursor id → remaining docs. Used
by `find` and `aggregate` to support pagination via `getMore` /
`killCursors`. Cursors carry a `last_access` timestamp; entries idle
longer than `idle_ttl_seconds` (default 600s, matching MongoDB's
10-minute cursor TTL) are pruned opportunistically. The clock is
injectable for deterministic testing.

#### `src/secantus/sortkey.py`

Pure `encode_value(v)` and `encode_compound([v1, v2, ...])` that produce
**byte-sortable** bytes whose lexicographic order matches MongoDB's BSON
cross-type sort order.

Layout: `<rank_byte><payload>`. Numbers go through a "lexical decimal"
form (sign byte + bias-shifted exponent + paired BCD digits + terminator)
so int / long / double / Decimal128 collide on equal value and order
correctly across the unified numeric type. NaN / ±Infinity get dedicated
bracketing markers.

`encode_value_directed(v, direction)` bitwise-inverts the bytes when
`direction == -1` so the same encoder drives descending indexes.

#### `src/secantus/storage.py` — `Storage`

Backed by **the same WiredTiger C library mongod ships** — vendored at
`vendor/wiredtiger/` (mongodb-7.0.33), built from source as part of the
wheel via scikit-build-core, and called via WT's official Python SWIG
bindings. There is no Python re-implementation of the storage engine:
B-tree pages, page eviction, checkpoint cadence, write-ahead logging,
durability, and the on-disk format are all pure WiredTiger. That's the
durability story — your data lives on the same battle-tested engine
mongod uses.

The four tables we keep in one WT connection:

| Table | Key format | Value | Purpose |
| --- | --- | --- | --- |
| `secantus_collections` | `SS` | BSON options blob | `(db, coll)` registry |
| `secantus_documents` | `SSu` | `bson.encode(doc)` | document store, keyed by `(db, coll, id_key)` |
| `secantus_indexes` | `SSS` | BSON `{key, options}` | index registry |
| `secantus_index_entries` | `SSSu` | `b""` | sortable index entries `(db, coll, name, packed)` |

`id_key` is `sortkey.encode_value(_id)`: byte-sortable across BSON types,
so iterating the doc table gives MongoDB's natural cross-type sort order.
`update_matching(multi=False)` and `find()` without `sort` walk in this
order, matching `mongod`.

For index entries, `packed = escape(sortkey) + b"\x00\x00" + id_key` —
the trailing `u` column packs both into one cell on purpose (WT
length-prefixes non-trailing `u` columns, which would break lex order).

WT sessions are thread-affine, kept in `threading.local()`; cursors per
session per table are cached and `reset()` between calls. A global
`RLock` serializes all public methods so we never have to think about
WT's MVCC at the storage layer.

`:memory:` is mapped to a `tempfile.mkdtemp()` opened with
`in_memory=true` and `rmtree`-cleaned on `close()`.

### Concurrency model

- **Server:** one daemon thread per accepted connection.
- **Storage:** all public methods acquire a global `RLock`; thread-safe
  by serialization, not by fine-grained locking. Fine for single-node
  workloads — write throughput is bounded by one writer at a time.
- **Cursors:** internal `Lock`, separate from storage.
- **Tests:** must run with `port=0` and a unique `storage_path` per
  test (we use pytest's `tmp_path` fixture). Real on-disk WiredTiger
  storage exercises the full schema; `:memory:` is reserved for the
  perf-regression suite where in-memory baselines control variance.
  Multiple `SecantusDBServer` instances coexist freely.

### Type-mapping strategy

Documents are stored as **opaque BSON blobs**. All filtering, projection,
sorting, and updates happen in Python after `bson.decode`. The storage
layer never inspects document content. This is deliberate: a `pymongo`
client cannot tell SecantusDB apart from `mongod` for the operations it
supports, and any lossy intermediate representation (JSON, native column
types, etc.) would break that for `ObjectId` / `Decimal128` /
`int32`-vs-`int64` / `Date`-with-tz / `Binary` / `Regex`.

Secondary indexes are typed **sort-key columns** derived from BSON
values via `sortkey.encode_value` — not JSON, not coerced numerics.

### Performance

Storage is fast (it's WiredTiger). The layers above storage are pure
Python. That trade-off shapes the performance profile.

A like-for-like benchmark — both servers running the same WT engine
on the same machine, driven by the same `pymongo` client over a TCP
socket — currently shows SecantusDB **8×–46× slower than mongod** per
operation. CRUD reads (`find` with an indexed equality / range) sit
near the lower end of that range; aggregation and bulk write
operations (`update_many`, `delete_many`) sit at the upper end where
Python-loop overhead dominates over the WT page reads / writes.

See [`docs/benchmark.md`](benchmark.md) for the current numbers and
the methodology to reproduce.

What this means for use:

- **Development and small data sets**: SecantusDB is a good choice.
  Per-op latency is in the hundreds of milliseconds for thousands of
  docs, which is fine when the alternative is installing and running a
  `mongod`.
- **Embedded single-node apps with modest throughput**: also a good
  fit. WT durability + on-disk format mean your data survives restart
  exactly the way it would under mongod.
- **High-throughput production replacement for mongod**: not yet, and
  honestly not the design goal. Hot-path Cython / native-code work in
  the command dispatcher and query planner is the obvious lever if
  the project ever decides to chase parity, but the current focus is
  conformance, not throughput.
