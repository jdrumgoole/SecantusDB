<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/jdrumgoole/SecantusDB/main/brandkit/wordmark-horizontal-on-dark.svg">
    <img src="https://raw.githubusercontent.com/jdrumgoole/SecantusDB/main/brandkit/wordmark-horizontal.svg" alt="SecantusDB — the SQLite of document databases" width="460">
  </picture>
</p>

[![Status: beta](https://img.shields.io/badge/status-beta-yellow)](#beta-software)
[![License: GPL-2.0-only (code) + CC-BY-4.0 (content)](https://img.shields.io/badge/license-GPL--2.0--only%20%2B%20CC--BY--4.0-blue)](#license)
[![Python: 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)](https://www.python.org/)
[![Documentation](https://img.shields.io/badge/docs-secantusdb.com-3b82f6)](https://secantusdb.com/docs/index.html)

> [!WARNING]
> **Beta software.** <a id="beta-software"></a>
>
> SecantusDB is past initial proving but its API surface (CLI flags,
> public class signatures) may still shift before 1.0. **The on-disk
> format is WiredTiger's** — the same engine MongoDB uses — and the
> schema we layer on top (collection / index / oplog tables) has been
> stable across releases; the test suite runs against real on-disk
> WiredTiger storage and the
> [persistence tests](https://github.com/jdrumgoole/SecantusDB/blob/main/tests/test_storage.py) explicitly verify
> close-and-reopen round-trips. That said, we don't yet ship a migration
> tool or a formal compatibility guarantee, so please don't put
> production data here yet — production deployments that need durable
> data across upgrades should still run a real `mongod` or `postgres`.

**A surrogate single-node database for your tests.** SecantusDB speaks
the real **MongoDB** and **PostgreSQL** wire protocols on a real TCP
socket, over the same **WiredTiger** storage engine MongoDB ships. Point
your existing driver at it — `pymongo`, `mongo-go-driver`, `mongosh`,
`psycopg`, `psql`, SQLAlchemy — and your application code doesn't know
the difference, as long as it only needs single-node behaviour. No
`mongod` or `postgres` to install, no port conflicts, parallel-test
friendly, embedded in-process or as a standalone daemon.

Two ways to get it, depending on which server you want.

**The Python servers** come from PyPI and start in-process in two lines
(the PostgreSQL one needs the `sql` extra):

```bash
pip install SecantusDB            # or "SecantusDB[sql]" for the PostgreSQL server
```

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(storage_path="./secantus-data") as server:  # port 0 = OS-assigned
    client = MongoClient(server.uri)
    db = client["mydb"]
    db["users"].insert_one({"_id": 1, "name": "Joe"})
    assert db["users"].find_one({"_id": 1})["name"] == "Joe"
```

```python
import psycopg
from secantus.sql import SecantusPGServer

with SecantusPGServer(storage_path="./secantus-pg-data") as server:
    with psycopg.connect(server.uri, autocommit=True) as conn:
        conn.execute("CREATE TABLE users (id int PRIMARY KEY, name text)")
        conn.execute("INSERT INTO users VALUES (1, 'Joe')")
        assert conn.execute("SELECT name FROM users WHERE id = 1").fetchone() == ("Joe",)
```

**The Rust servers** are the fast ones, and they ship as Rust crates on
crates.io, not inside the Python wheel:

```bash
cargo install secantus-mdb --version 0.5.3-beta.177   # MongoDB: the `secantusd-rs` daemon
cargo install secantus-pg --version 0.1.0-beta.7      # PostgreSQL: the `secantusd-pg` daemon
secantusd-rs --port 27017 --storage-path ./secantus-data
```

Both crates are pre-releases, so `cargo install` needs the `--version`:
without it cargo stops at "nothing to install". The current versions are on
the [`secantus-mdb`](https://crates.io/crates/secantus-mdb) and
[`secantus-pg`](https://crates.io/crates/secantus-pg) pages. Each crate is
also a library: `secantus_mdb::Server` and `secantus_pg::PgServer` start a
server inside a Rust test. Prebuilt archives of both Rust servers are attached
to their tags on [GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases).

Single-node only by design: replica sets, sharding, streaming
replication, and anything else that depends on real cluster topology are
out of scope. Within that scope, SecantusDB is the database your driver
thinks it's talking to — same handshake, same wire frames, same error
codes.

## The four servers

Two wire protocols, two implementations of each, one storage format.
**Reach for Rust to run it; reach for Python to read it.**

| Server | Wire | Run it as | Role |
| --- | --- | --- | --- |
| **Rust MongoDB server** | MongoDB | `secantusd-rs` / `secantus_mdb::Server` | **The flagship.** The [`secantus-mdb`](https://crates.io/crates/secantus-mdb) crate; prebuilt binaries per platform |
| **Rust PostgreSQL server** | PostgreSQL | `secantusd-pg` | **The newest.** Prebuilt `secantusd-pg` binaries on [GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases), and the [`secantus-pg`](https://crates.io/crates/secantus-pg) crate |
| Python MongoDB server | MongoDB | `SecantusDBServer` / `secantusd-py` | The readable reference — every operator, stage and error message lands here first |
| Python PostgreSQL server | PostgreSQL | `SecantusPGServer` / `secantusd-py-pg` | The reference for the SQL surface, and still the most complete one |

Each server is held to the **real product** it imitates — `mongod` for
the two MongoDB servers, PostgreSQL for the two SQL servers — never to
another SecantusDB server. See
[The two servers](https://secantusdb.com/docs/servers.html) for what each
one does not support yet.

**Sharing one store.** All four read and write the same on-disk format,
but how they can share it depends on the pair:

- The **Python pair** can share one `Storage` object in one process
  (`SecantusPGServer(..., storage=mongo_server.storage)`) and serve both
  protocols at once: a collection written with `pymongo` is queryable as
  a SQL table, `JOIN`s and all, with no `CREATE TABLE`.
- **Separate processes** — including the two Rust servers — can only
  **hand a directory over**, never serve it concurrently. WiredTiger
  takes an exclusive file lock, so a second server pointed at a directory
  that is already open exits rather than opening it. Between the Rust
  pair the hand-over works in one direction only: a table written by
  `secantusd-pg` reads back through `secantusd-rs` as a collection, but a
  `pymongo`-written collection is not visible to the Rust PostgreSQL
  server until something runs `CREATE TABLE` for it.

## Storage engine

SecantusDB uses **the same WiredTiger C library mongod ships** —
vendored at `vendor/wiredtiger/` (mongodb-7.0.33) and built from source
into the wheel. There is no re-implementation of the storage engine:
B-trees, page eviction, write-ahead logging, durability, and on-disk
format are all WiredTiger's.

The layers above storage — command dispatch, query planning, the
operator engines — are where the servers differ. On a like-for-like
benchmark the **Rust MongoDB server runs within 1.0×–3.2× of `mongod`**
per operation across three runs (reads at the low end, multi-stage
aggregation at the high end); the Python server runs 2×–27×. See
[`docs/benchmark.md`](https://secantusdb.com/docs/benchmark.html) for
the numbers and methodology. The right use is tests, dev, CI,
containers, and single-node prototypes where conformance and
WiredTiger durability matter more than per-operation latency.

## What's in scope: MongoDB

Everything a single-node application needs from the wire — the
handshake (`hello` / `isMaster` / `ping` / `buildInfo` / ...), CRUD
(`insert` / `find` / `update` / `delete` / `findAndModify` / `count` /
`drop`), cursors with `getMore` / `killCursors`, aggregation pipelines
and the expression language they need, multi-document transactions, and
**change streams** (single-node, oplog-backed; collection / db / cluster
scope; resume tokens; `fullDocument: "updateLookup"`; pre-images via
`fullDocumentBeforeChange`; blocking `awaitData` getMore). All backed by
a real query planner with **index acceleration** — single-field,
compound, mixed-direction, multikey, partial, sparse, TTL, sort —
`explain` output (`IXSCAN` vs `COLLSCAN`), and geo support
(`$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`, `$geoNear`,
`2dsphere` and `2d` indexes).

The target is **mongod 8.x**, and the Rust server passes **99.5%** of
pymongo's own unmodified test suite (1,205 of 1,210 run, 2026-09-28).
Twelve other official drivers run their suites against it too — see the
[conformance validation summary](https://secantusdb.com/docs/validation-summary.html).

**Security**: SCRAM-SHA-256 authentication, the **MONGODB-X509**
cert-as-username mechanism, native **TLS / mTLS**, and role-based
**authorization** with mongod's built-in roles. All off by default; turn
auth on with `--auth` (or `require_auth=True`), provision users with
`createUser`, then connect with the standard
`MongoClient(uri, username=, password=)` shape. See
[Authentication](https://secantusdb.com/docs/authentication.html).

What's **out of scope:** real replica sets, sharding, auth mechanisms
beyond SCRAM / MONGODB-X509 (no LDAP / Kerberos / GSSAPI / AWS / OIDC),
`OP_COMPRESSED`, text / hashed / wildcard indexes, and `$where` /
`$function` / `$accumulator` / `mapReduce` (no embedded JS runtime). If
you need those, run a real `mongod`.

## What's in scope: PostgreSQL

The subset of the PostgreSQL wire protocol real clients use — the
extended query protocol (Parse / Bind / Describe / Execute), prepared
statements and portals, text *and* binary formats, transactions with
savepoints and two-phase commit, `COPY`, server-side cursors,
`LISTEN` / `NOTIFY`, and the catalog tables a client introspects.

The **Rust PostgreSQL server** parses SQL with `libpg_query` — the real
PostgreSQL grammar. Of the 5,731 tests of psycopg 3's own unmodified
suite that ran against it on 2026-10-10 (macOS, psycopg 3.3.4), it passes
**5,544 and fails none**; psycopg skips 149 and expects 34 to fail. That
gauge measures the protocol and the type system. The query language now
covers joins, correlated subqueries and CTEs, window functions, `GROUP BY`
with `GROUPING SETS`, views, indexes, `ALTER TABLE`, `ON CONFLICT`,
`MERGE`, triggers and PL/pgSQL functions. Not yet: TLS, a foreign table's
rows, and a collection written through the MongoDB server read as a table.
`EXPLAIN` prints a plan with zero costs. What it does not do it refuses
with SQLSTATE `0A000` rather than answering wrongly.

The **Python PostgreSQL server** has the wider SQL surface, including
schema-on-read over MongoDB collections (nested documents surface as
`jsonb` with `->`, `->>`, `#>`). It needs the `sql` extra:

```bash
pip install "SecantusDB[sql]"
secantusd-py-pg --host 127.0.0.1 --port 5432 --storage-path ./secantus-data
```

See [SQL / PostgreSQL interface](https://secantusdb.com/docs/sql.html)
for the supported-SQL matrix and examples.

## Installation

```bash
pip install SecantusDB
```

Pre-built wheels are published for CPython **3.10**, **3.11**, **3.12**, and **3.13** on:

- macOS arm64 (Apple Silicon)
- Linux x86_64 and aarch64 (manylinux_2_28 / glibc, and musllinux_1_2 / Alpine)
- Windows AMD64

Each wheel carries both Python servers and WiredTiger itself — no separate
package, no compile step, no system build tools required. It does **not**
carry the Rust servers (see below). macOS Intel (x86_64) is not in the
wheel matrix.

Both Rust servers install from crates.io: `cargo install secantus-mdb
--version 0.5.3-beta.177` and `cargo install secantus-pg --version 0.1.0-beta.7`.
Each needs a Rust toolchain and builds WiredTiger as part of the crate.

Standalone archives of `secantusd-rs` (Linux x86_64, macOS arm64,
Windows x86_64) and `secantusd-pg` (Linux x86_64, macOS arm64) are
attached to the `secantusdb-v*` and `secantusd-pg-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases), for
when you want the server without Python at all.

### Building from source (unsupported platforms only)

If your platform isn't in the matrix above, `pip install SecantusDB`
falls back to the sdist and compiles WiredTiger from source. That
needs three native build tools on `PATH`:

- **`cmake`** (>= 3.21)
- **`ninja`**
- **`swig`** (>= 4.0)

| Platform | Install prerequisites |
|---|---|
| macOS (Homebrew) | `brew install cmake ninja swig` |
| Debian/Ubuntu | `sudo apt-get install -y cmake ninja-build swig` |
| Fedora/RHEL | `sudo dnf install -y cmake ninja-build swig` |
| Alpine | `apk add --no-cache cmake ninja swig build-base` |

See [Installation](https://secantusdb.com/docs/installation.html) for dev-install instructions.

## Standalone daemons (drop-in `mongod` / `postgres` replacements)

Installing the `secantus-mdb` crate puts `secantusd-rs` on your `PATH` (or
unpack a release archive). Run it like you'd run `mongod`:

```bash
secantusd-rs --host 127.0.0.1 --port 27017 --storage-path ./secantus-data
```

The Rust PostgreSQL server comes from the `secantus-pg` crate or a release
archive, and takes positional arguments:

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432
```

Then point any driver or tool at it — **no application code changes**,
just the connection string:

```bash
mongosh mongodb://127.0.0.1:27017
mongodump --uri mongodb://127.0.0.1:27017 --out ./dump
psql "host=127.0.0.1 port=5432 dbname=postgres user=postgres"
```

The Python reference servers run the same way as `secantusd-py` and
`secantusd-py-pg`.

## Examples

A walk through the operations a typical application exercises — connect,
insert, index, query, drop. Full version with explanations: [examples in
the docs](https://secantusdb.com/docs/examples.html).

```python
import tempfile

from pymongo import MongoClient
from secantus import SecantusDBServer

# A throwaway directory so the snippet is self-contained; pass a real
# path to keep the data across restarts.
with SecantusDBServer(storage_path=tempfile.mkdtemp()) as server:
    client = MongoClient(server.uri)
    cellar = client["wine_cellar"]
    bottles = cellar["bottles"]

    # --- Insert ---
    bottles.insert_one(
        {"_id": 1, "name": "Pommard 2018", "region": "Burgundy", "year": 2018}
    )
    bottles.insert_many(
        [
            {"_id": 2, "name": "Brunello 2015", "region": "Tuscany", "year": 2015},
            {"_id": 3, "name": "Barolo 2017", "region": "Piedmont", "year": 2017},
            {"_id": 4, "name": "Pommard 2020", "region": "Burgundy", "year": 2020},
        ]
    )

    # --- Indexes ---
    bottles.create_index([("year", 1)])                     # single-field
    bottles.create_index([("region", 1), ("year", -1)])     # compound

    # --- Query ---
    drinkable_now = list(
        bottles.find({"year": {"$lte": 2018}}).sort("year")
    )
    assert [b["name"] for b in drinkable_now] == [
        "Brunello 2015",
        "Barolo 2017",
        "Pommard 2018",
    ]

    by_region = list(
        bottles.aggregate(
            [
                {"$group": {"_id": "$region", "count": {"$sum": 1}}},
                {"$sort": {"_id": 1}},
            ]
        )
    )

    # --- Drop ---
    bottles.drop()                              # one collection
    client.drop_database("wine_cellar")         # whole database
```

## Admin web UI

An optional local console — browse collections, watch live metrics, tail
change streams, inspect query plans, manage users, and take backups
(including point-in-time recovery) against any SecantusDB or
MongoDB-wire server you already have running.

```bash
pip install 'SecantusDB[admin]'
secantus-admin --uri mongodb://127.0.0.1:27017
```

<img src="https://raw.githubusercontent.com/jdrumgoole/SecantusDB/main/docs/screenshots/admin-dashboard.png" alt="The SecantusDB admin dashboard: live server metrics, operation counters and per-second charts." width="900">

It's dev-tool shaped, not a production console: loopback-only, gated by a
local token, with every script and stylesheet served from the package.
See [the admin UI docs](https://secantusdb.com/docs/admin.html) for a
tour of all 22 pages.

## Documentation

Full docs are at [secantusdb.com/docs](https://secantusdb.com/docs/index.html) — with the Rust server's own tree at [secantusdb.com/docs/rust](https://secantusdb.com/docs/rust/index.html).
Highlights:

- [Quickstart](https://secantusdb.com/docs/quickstart.html) — embedding in tests, running standalone.
- [The two servers](https://secantusdb.com/docs/servers.html) — Rust vs Python server, and what each doesn't support yet.
- [SQL / PostgreSQL interface](https://secantusdb.com/docs/sql.html) — the supported SQL surface.
- [Architecture](https://secantusdb.com/docs/architecture.html) — the layered design.
- [Indexes](https://secantusdb.com/docs/indexes.html) — what `find()` and `aggregate` accelerate,
  `explain` semantics, hints, partial indexes, TTL.
- [Aggregation](https://secantusdb.com/docs/aggregation.html) — supported pipeline stages and
  expression operators.
- [Compatibility](https://secantusdb.com/docs/compatibility.html) — the divergences you should know
  about before you point an application at SecantusDB.
- [Conformance validation](https://secantusdb.com/docs/validation-summary.html) — each
  official driver's own test suite run **unmodified** against SecantusDB,
  with a cross-driver summary table and a per-driver report. The
  other-language gauges catch wire-protocol bugs that pymongo's
  permissive client accepts silently (e.g. int32-vs-int64 cursor ids).

## Development

```bash
git clone https://github.com/jdrumgoole/SecantusDB.git
cd SecantusDB
git submodule update --init vendor/wiredtiger
./inv sync                     # uv sync --all-extras, rebuilding the Rust core
uv run python -m pytest        # runs in parallel under pytest-xdist
```

Common workflows:

```bash
uv run python -m invoke fmt    # ruff format
uv run python -m invoke lint   # ruff check
uv run python -m invoke test   # pytest, parallel
uv run python -m invoke docs   # build Sphinx docs (warnings as errors)
```

## License

SecantusDB is dual-licensed:

- **Code** — GPL-2.0-only. See [`LICENSE`](https://github.com/jdrumgoole/SecantusDB/blob/main/LICENSE). SecantusDB bundles
  the [WiredTiger](https://github.com/wiredtiger/wiredtiger) storage
  engine (itself GPL-2/GPL-3), so the combined work is GPL.
- **Written content** — [Creative Commons Attribution 4.0
  International (CC-BY 4.0)](https://creativecommons.org/licenses/by/4.0/).
  See [`LICENSE-DOCS`](https://github.com/jdrumgoole/SecantusDB/blob/main/LICENSE-DOCS). Covers `README.md`, everything
  under `docs/`, the validation reports, and `pymongo_validation/README.md`.
  Operational instructions to AI assistants (`CLAUDE.md`) and vendored
  third-party content (under `vendor/`) are out of scope.
