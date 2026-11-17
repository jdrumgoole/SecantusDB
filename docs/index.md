# SecantusDB

:::{warning}
**Beta software.**

SecantusDB is past initial proving but its interfaces (command-line
flags, the `secantusdb` launcher, the Rust crates' public API) may still
shift before 1.0. **The on-disk format is WiredTiger's** — the same
engine MongoDB uses — and the schema we layer on top (collection / index
/ oplog tables) has been stable across releases; the test suite runs
against real on-disk WiredTiger storage and the persistence tests
explicitly verify close-and-reopen round-trips. That said, we don't yet
ship a migration tool or a formal compatibility guarantee, so please
don't put production data here yet — production deployments that need
durable data across upgrades should still run a real `mongod` or
`postgres`.
:::

**A single-node MongoDB and PostgreSQL database, written in Rust.**
SecantusDB is two real database servers over the same
[WiredTiger](https://github.com/wiredtiger/wiredtiger) storage engine
`mongod` ships:

- **`secantusd-mdb`** speaks the MongoDB wire protocol on a real TCP
  socket, so any standard MongoDB driver or tool —
  [`pymongo`](https://pymongo.readthedocs.io/en/stable/),
  [`mongo-go-driver`](https://github.com/mongodb/mongo-go-driver),
  `mongosh`, `mongodump` / `mongorestore` — connects unchanged.
- **`secantusd-pg`** (beta) speaks the PostgreSQL wire protocol, so
  `psql`, `psycopg`, SQLAlchemy and other PostgreSQL clients connect
  unchanged.

Point a client at either and your application code doesn't know the
difference, as long as it only needs single-node behaviour.

Three ways to get them (see [Installation](installation.md)):

- **PyPI** — `pip install SecantusDB` puts both prebuilt binaries on
  `PATH` and adds the `secantusdb` Python launcher (Linux x86_64, macOS
  arm64).
- **crates.io** — `cargo install secantus-mdb` and `cargo install
  secantus-pg` build each server from source; each crate is also a Rust
  library (`secantus_mdb::Server`, `secantus_pg::PgServer`).
- **GitHub Releases** — prebuilt archives of each server.

Run either one as a daemon, or start it from your own program:

```bash
secantusd-mdb --host 127.0.0.1 --port 27017 --storage-path ./secantus-data
secantusd-pg ./secantus-pg-data 127.0.0.1:5432
```

```python
from pymongo import MongoClient
from secantusdb import MongoServer

with MongoServer() as server:        # a free port and a temporary data directory
    client = MongoClient(server.uri)
    db = client["mydb"]
    db["users"].insert_one({"_id": 1, "name": "Joe"})
    assert db["users"].find_one({"_id": 1})["name"] == "Joe"
```

Single-node only by design. Replica sets, sharding, and anything that
depends on real cluster topology are out of scope — but within the
single-node scope, SecantusDB is the database your driver thinks it's
talking to: same handshake, same wire frames, same error codes.

The Rust server also has its own documentation tree, covering running the
daemon, configuration, TLS, recovery and embedding, at
[secantusdb.com/docs/rust](https://secantusdb.com/docs/rust/index.html).
The original Python servers are fully deprecated and kept only as a record:
[Deprecated: Python servers](python-servers.md).

Connect with **any** MongoDB driver or tool, unchanged:

```bash
mongosh mongodb://127.0.0.1:27017
mongodump --uri mongodb://127.0.0.1:27017 --out ./dump
```

```python
from pymongo import MongoClient
client = MongoClient("mongodb://127.0.0.1:27017")  # same code as for mongod
```

The conformance evidence: the official test suites for **pymongo**,
**mongo-go-driver**, **mongo-node-driver**, **mongo-java-driver**,
**mongo-ruby-driver**, **mongo-rust-driver**, and the **PHP**
library + extension all run against
SecantusDB **unmodified** — see the
[pymongo](validation-report.md),
[pymongo-async](validation-report-pymongo-async.md),
[Go-driver](validation-report-go.md),
[Node-driver](validation-report-node.md),
[Java-driver](validation-report-java.md),
[Kotlin-driver](validation-report-kotlin.md),
[Ruby-driver](validation-report-ruby.md),
[Rust-driver](validation-report-rust.md),
[PHP-library](validation-report-php-lib.md),
[PHP-extension](validation-report-php-ext.md),
[C-driver](validation-report-c.md),
[C++-driver](validation-report-cxx.md), and
[.NET-driver](validation-report-dotnet.md) validation reports for
current pass-rates per feature category.

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
- **Authentication and authorization** — SCRAM-SHA-256 / SCRAM-SHA-1 and
  MONGODB-X509 over the standard wire protocol, the full user / role admin
  command set, and enforced RBAC (built-in and custom roles). Off by
  default; flip on with `--auth`. See
  [Authentication](authentication.md).

See [Indexes](indexes.md) and [Aggregation](aggregation.md) for the full
inventory, and [Compatibility](compatibility.md) for what's intentionally
not supported.

## What's out of scope

Anything that depends on **real** cluster topology — multi-node replica
sets, sharding, election, cross-node oplog — and the infrastructure
features tangential to single-node operation:

- Authentication mechanisms beyond SCRAM and MONGODB-X509 (LDAP,
  Kerberos, GSSAPI, MONGODB-AWS, MONGODB-OIDC). SCRAM-SHA-256 /
  SCRAM-SHA-1, MONGODB-X509, and enforced RBAC **are** supported — see
  [Authentication](authentication.md).
- Text search (no full-text index).
- `$where` (no JS runtime).

If your application depends on those features, run a real `mongod`.
SecantusDB is the right tool when you need a single-node MongoDB server —
embedded in a process, run as a daemon, or shared across multiple language
runtimes — without the clustering and infrastructure that `mongod` brings.

## Quick links

- [Installation](installation.md) — PyPI, crates.io and prebuilt binaries.
- [Quickstart](quickstart.md) — run either server as a daemon or start it from your own code.
- [Examples](examples.md) — connect, insert, index, query, drop.
- [The servers](servers.md) — the two Rust servers, their versions and what
  each doesn't support yet.
- [Rust server docs](https://secantusdb.com/docs/rust/index.html) — running,
  configuring, securing, recovering and embedding `secantusd-mdb`.
- [Deprecated: Python servers](python-servers.md) — the original Python
  servers, kept as a record.
- [Architecture](architecture.md) — wire / commands / query / aggregate /
  storage layers.
- [Indexes](indexes.md) — single-field, compound, mixed-direction, partial,
  TTL, hint, sort acceleration, `explain`.
- [Geospatial](geospatial.md) — `2d` and `2dsphere` indexes,
  `$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`,
  `$geoNear`, distance-unit conventions across spec forms.
- [Aggregation](aggregation.md) — supported pipeline stages and expression
  operators.
- [Change streams](change-streams.md) — `watch()` against SecantusDB,
  resume tokens, pre-images, retention.
- [Authentication](authentication.md) — SCRAM-SHA-256, `--auth`,
  `createUser` / `dropUser` / `usersInfo`.
- [Admin web UI](admin.md) — `secantus-admin` console, page tour,
  security model, programmatic use.
- [Backup & point-in-time recovery](recovery.md) — how PITR works
  (oplog model, the applier, manifests), snapshot backups, restoring to
  a target timestamp, arbitrary-window archiving, change-stream resume
  continuity, and cross-server compatibility — for both servers.
- [Running in production](production.md) — honest comparison vs
  single-node Postgres, what's missing for production use, and a
  concrete `systemd` / TLS / backup / monitoring deployment shape.
- [Concurrency](concurrency.md) — what scales, what doesn't, and why
  multi-writer throughput is capped by WiredTiger.
- [Compatibility](compatibility.md) — known divergences from real MongoDB.
- [Feature comparison](feature-comparison.md) — MongoDB vs the Rust server
  (and the deprecated Python server), feature by feature.
- [API reference](api.md) — the deprecated Python server's `SecantusDBServer` and `Storage`.

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
servers
python-servers
configuration
architecture
indexes
geospatial
aggregation
sql
change-streams
authentication
admin
opsboard
recovery
production
concurrency
compatibility
feature-comparison
benchmark
validation-summary
validation-report
validation-report-pymongo-async
validation-report-go
validation-report-node
validation-report-java
validation-report-kotlin
validation-report-ruby
validation-report-rust
validation-report-php-lib
validation-report-php-ext
validation-report-c
validation-report-cxx
validation-report-dotnet
validation-report-psycopg
validation-report-slt
validation-report-sqlalchemy
validation-report-pgx
validation-report-sqlstress
validation-report-pgtest
validation-report-pgjdbc
validation-report-rust-server
validation-report-pymongo-async-rust-server
validation-report-go-rust-server
validation-report-node-rust-server
validation-report-java-rust-server
validation-report-kotlin-rust-server
validation-report-ruby-rust-server
validation-report-rust-rust-server
validation-report-php-lib-rust-server
validation-report-php-ext-rust-server
validation-report-c-rust-server
validation-report-cxx-rust-server
validation-report-dotnet-rust-server
validation-report-psycopg-rust-server
validation-report-slt-rust-server
validation-report-sqlalchemy-rust-server
validation-report-pgx-rust-server
validation-report-sqlstress-rust-server
validation-report-pgtest-rust-server
validation-report-pgjdbc-rust-server
api
changelog
```
