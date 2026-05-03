# SecantusDB

:::{warning}
**Alpha software — not for production use.**

SecantusDB is in early development. The API surface and on-disk format
can change between point releases. It exists today as a developer tool
for exercising application code against a MongoDB-shaped surrogate; it
is **not** a database for storing data you can't recreate.
:::

A **surrogate single-node MongoDB server** in Python. SecantusDB speaks the
subset of the MongoDB wire protocol that
[`pymongo`](https://pymongo.readthedocs.io/en/stable/) emits, so application
test suites can talk to it instead of standing up a real `mongod`.

The audience is developers who want **fast, ephemeral, in-process MongoDB
behaviour for tests**. SecantusDB is intentionally scoped to single-node
operation — replica sets, sharding, and other cluster-only features are out
of scope by design — so it stays simple, fast, and easy to embed.

## What it is

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    client = MongoClient(server.uri)
    db = client["mydb"]
    db["users"].insert_one({"_id": 1, "name": "Joe"})
    assert db["users"].find_one({"_id": 1})["name"] == "Joe"
```

That's it. No `mongod` to install, no port conflicts, no test isolation
gymnastics — every `SecantusDBServer(port=0)` gets its own OS-assigned port
and its own `:memory:` WiredTiger storage, so suites run cleanly under
`pytest-xdist`.

## What's in scope

The subset of MongoDB that `pymongo` actually drives:

- Connection handshake (`hello` / `isMaster` / `ping` / `buildInfo` / ...).
- CRUD: `insert`, `find`, `update`, `delete`, `findAndModify`, `count`, `drop`.
- Cursors with `getMore` / `killCursors` (with a 10-minute idle TTL).
- Aggregation pipelines and the expression language they need.
- Indexes — single-field, compound, mixed-direction, partial, TTL — with a
  real query planner that chooses the right index and reports it through
  `explain`.

See [Indexes](indexes.md) and [Aggregation](aggregation.md) for the full
inventory, and [Compatibility](compatibility.md) for what's intentionally
not supported.

## What's out of scope

Anything that depends on cluster topology — replica sets, sharding, change
streams, oplog tailing — and anything tangential to test-harness use:

- Authentication (SCRAM / x509 / LDAP / Kerberos).
- TLS / SSL.
- Text search and geo indexes.
- `$where` (no JS runtime).
- Real transaction rollback (we accept `commitTransaction` /
  `abortTransaction` but operations take effect immediately).

If a test depends on those features, it needs a real `mongod`. SecantusDB
is the right tool when the test is exercising your application's MongoDB
usage, not the database's clustering or auth.

## Quick links

- [Installation](installation.md) — `pip install` and the WiredTiger build
  prerequisites.
- [Quickstart](quickstart.md) — embedding in tests, running standalone.
- [Examples](examples.md) — connect, insert, index, query, drop.
- [Architecture](architecture.md) — wire / commands / query / aggregate /
  storage layers.
- [Indexes](indexes.md) — single-field, compound, mixed-direction, partial,
  TTL, hint, sort acceleration, `explain`.
- [Aggregation](aggregation.md) — supported pipeline stages and expression
  operators.
- [Compatibility](compatibility.md) — known divergences from real MongoDB.
- [API reference](api.md) — `SecantusDBServer`, `Storage`, public surface.

## License

GPL-2.0-only. SecantusDB depends on (and intends to bundle) the
[WiredTiger](https://github.com/wiredtiger/wiredtiger) storage engine,
which is itself GPL-2/GPL-3, so the combined work is GPL.

```{toctree}
:maxdepth: 2
:caption: Contents
:hidden:

installation
quickstart
examples
architecture
indexes
aggregation
compatibility
api
```
