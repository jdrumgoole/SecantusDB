# SecantusDB

:::{warning}
**Beta software.**

SecantusDB is past initial proving but the Python API surface (CLI
flags, public class signatures) may still shift before 1.0. **The
on-disk format is WiredTiger's** — the same engine MongoDB uses —
and the schema we layer on top (collection / index / oplog tables)
has been stable across releases; the test suite runs against real
on-disk WiredTiger storage and the persistence tests explicitly
verify close-and-reopen round-trips. That said, we don't yet ship a
migration tool or a formal compatibility guarantee, so please don't
put production data here yet — production deployments that need
durable data across upgrades should still run a real `mongod`.
:::

**Drop-in MongoDB for single-node applications.** SecantusDB is a real
MongoDB server in Python: it speaks the MongoDB wire protocol on the
same TCP socket a `mongod` would, so any standard MongoDB driver or tool —
[`pymongo`](https://pymongo.readthedocs.io/en/stable/),
[`mongo-go-driver`](https://github.com/mongodb/mongo-go-driver), `mongosh`,
`mongodump` / `mongorestore` — connects unchanged. Point a `MongoClient`
at it and your application code doesn't know the difference, as long as
the application only needs single-node behaviour.

There are two ways to use it:

- **Embedded** — `with SecantusDBServer(...) as server:` inside your
  application or test process. No subprocess, no port collision (use
  `port=0`), no separate daemon to manage. Also ideal for tests under
  `pytest-xdist`.
- **Standalone daemon** — `pip install` puts a `secantusdb` script on
  `PATH` that runs the server like a `mongod`. Drop-in replacement for a
  single-node `mongod` in dev / CI / small production-shaped workloads
  where the beta caveats are acceptable.

Single-node only by design. Replica sets, sharding, and anything that
depends on real cluster topology are out of scope — but within the
single-node scope, SecantusDB is the database your driver thinks it's
talking to: same handshake, same wire frames, same error codes.

## Embedded

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

# On-disk by default at ./secantus-data; ":memory:" for ephemeral.
with SecantusDBServer(port=27017) as server:
    client = MongoClient(server.uri)
    db = client["mydb"]
    db["users"].insert_one({"_id": 1, "name": "Joe"})
    assert db["users"].find_one({"_id": 1})["name"] == "Joe"
```

That's it. No `mongod` to install. The on-disk store keeps your data
across restarts; for tests, swap in `port=0, storage_path=":memory:"`
so each `SecantusDBServer` instance gets an OS-assigned port and an
ephemeral WiredTiger home — suites then run cleanly under `pytest-xdist`.

## Standalone daemon

```bash
secantusdb --host 127.0.0.1 --port 27017
# storage at ./secantus-data by default
```

Then connect with **any** MongoDB driver or tool, unchanged:

```bash
mongosh mongodb://127.0.0.1:27017
mongodump --uri mongodb://127.0.0.1:27017 --out ./dump
```

```python
from pymongo import MongoClient
client = MongoClient("mongodb://127.0.0.1:27017")  # same code as for mongod
```

The conformance evidence: the official test suites for **pymongo**,
**mongo-go-driver**, **mongo-node-driver**, **mongo-java-driver**, and
**mongo-ruby-driver** all run against SecantusDB **unmodified** — see
the [pymongo](validation-report.md), [Go-driver](validation-report-go.md),
[Node-driver](validation-report-node.md),
[Java-driver](validation-report-java.md), and
[Ruby-driver](validation-report-ruby.md) validation reports for current
pass-rates per feature category.

## What's in scope

Everything a single-node application needs from the wire:

- Connection handshake (`hello` / `isMaster` / `ping` / `buildInfo` / ...).
- CRUD: `insert`, `find`, `update`, `delete`, `findAndModify`, `count`, `drop`.
- Cursors with `getMore` / `killCursors` (with a 10-minute idle TTL).
- Aggregation pipelines and the expression language they need.
- Indexes — single-field, compound, mixed-direction, partial, TTL — with a
  real query planner that chooses the right index and reports it through
  `explain`.
- Change streams — single-node, oplog-backed; collection / db / cluster
  scope; resume tokens; `fullDocument: "updateLookup"`; pre-images via
  `fullDocumentBeforeChange`; blocking `awaitData` getMore.
- **Authentication** — SCRAM-SHA-256 over the standard wire protocol,
  with `createUser` / `dropUser` / `usersInfo` admin commands and
  per-connection state. Off by default; flip on with `--auth` /
  `require_auth=True`. See [Authentication](authentication.md).
  (Authorization / RBAC is *not* enforced — an authenticated principal
  is treated as fully privileged.)

See [Indexes](indexes.md) and [Aggregation](aggregation.md) for the full
inventory, and [Compatibility](compatibility.md) for what's intentionally
not supported.

## What's out of scope

Anything that depends on **real** cluster topology — multi-node replica
sets, sharding, election, cross-node oplog — and the infrastructure
features tangential to single-node operation:

- Authentication mechanisms beyond SCRAM-SHA-256 (x509, LDAP, Kerberos,
  GSSAPI, MONGODB-AWS, MONGODB-OIDC). SCRAM-SHA-256 itself **is**
  supported — see [Authentication](authentication.md).
- Authorization (RBAC) — `createUser` accepts a `roles` array but no
  command consults it; an authenticated principal is treated as fully
  privileged.
- TLS / SSL.
- Text search and geo indexes.
- `$where` (no JS runtime).
- Real transaction rollback (we accept `commitTransaction` /
  `abortTransaction` but operations take effect immediately).

If your application or test depends on those features, run a real
`mongod`. SecantusDB is the right tool when you need a single-node
MongoDB server — embedded in a process, run as a daemon for dev / CI, or
shared across multiple language runtimes — without standing up the
clustering and infrastructure that `mongod` brings.

## Quick links

- [Installation](installation.md) — `pip install` and the WiredTiger build
  prerequisites.
- [Quickstart](quickstart.md) — embedded in your app, run as a daemon, or wired into tests.
- [Examples](examples.md) — connect, insert, index, query, drop.
- [Architecture](architecture.md) — wire / commands / query / aggregate /
  storage layers.
- [Indexes](indexes.md) — single-field, compound, mixed-direction, partial,
  TTL, hint, sort acceleration, `explain`.
- [Aggregation](aggregation.md) — supported pipeline stages and expression
  operators.
- [Change streams](change-streams.md) — `watch()` against SecantusDB,
  resume tokens, pre-images, retention.
- [Authentication](authentication.md) — SCRAM-SHA-256, `--auth`,
  `createUser` / `dropUser` / `usersInfo`.
- [Admin web UI](admin.md) — `secantusdb-admin` console, page tour,
  security model, programmatic use.
- [Compatibility](compatibility.md) — known divergences from real MongoDB.
- [API reference](api.md) — `SecantusDBServer`, `Storage`, public surface.

## License

SecantusDB is dual-licensed:

- **Code** — GPL-2.0-only. SecantusDB bundles the
  [WiredTiger](https://github.com/wiredtiger/wiredtiger) storage engine,
  which is itself GPL-2/GPL-3, so the combined work is GPL.
- **Written content** — [Creative Commons Attribution 4.0
  International](https://creativecommons.org/licenses/by/4.0/) (CC-BY
  4.0). Covers this documentation, the project README, the validation
  reports, and `pymongo_validation/README.md`. Reuse and adapt freely
  with attribution.

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
change-streams
authentication
admin
compatibility
benchmark
validation-report
validation-report-go
validation-report-node
validation-report-java
validation-report-ruby
api
```
