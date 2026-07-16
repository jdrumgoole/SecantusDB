<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/jdrumgoole/SecantusDB/main/brandkit/wordmark-horizontal-on-dark.svg">
    <img src="https://raw.githubusercontent.com/jdrumgoole/SecantusDB/main/brandkit/wordmark-horizontal.svg" alt="SecantusDB — the SQLite of document databases" width="460">
  </picture>
</p>

[![Status: beta](https://img.shields.io/badge/status-beta-yellow)](#beta-software)
[![Tests: 584 passing](https://img.shields.io/badge/tests-584%20passing-brightgreen)](#)
[![License: GPL-2.0-only (code) + CC-BY-4.0 (content)](https://img.shields.io/badge/license-GPL--2.0--only%20%2B%20CC--BY--4.0-blue)](#license)
[![Python: 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)](https://www.python.org/)
[![Documentation](https://img.shields.io/badge/docs-secantusdb.com-3b82f6)](https://secantusdb.com/docs/index.html)

> [!WARNING]
> **Beta software.** <a id="beta-software"></a>
>
> SecantusDB is past initial proving but the Python API surface (CLI
> flags, public class signatures) may still shift before 1.0. **The
> on-disk format is WiredTiger's** — the same engine MongoDB uses —
> and the schema we layer on top (collection / index / oplog tables)
> has been stable across releases; the test suite runs against real
> on-disk WiredTiger storage and the
> [persistence tests](https://github.com/jdrumgoole/SecantusDB/blob/main/tests/test_storage.py) explicitly verify
> close-and-reopen round-trips. That said, we don't yet ship a migration
> tool or a formal compatibility guarantee, so please don't put
> production data here yet — production deployments that need durable
> data across upgrades should still run a real `mongod`.

**Drop-in MongoDB for single-node applications.** SecantusDB is a real
MongoDB server written in Python: it speaks the MongoDB wire protocol on
the same TCP socket a `mongod` would, so any standard MongoDB driver or
tool — [`pymongo`](https://pymongo.readthedocs.io/en/stable/),
[`mongo-go-driver`](https://github.com/mongodb/mongo-go-driver),
`mongosh`, `mongodump` / `mongorestore` — connects unchanged. Point a
`MongoClient` at it and your application code doesn't know the
difference, as long as the application only needs single-node behaviour.
No `mongod` to install, no port conflicts, parallel-test friendly,
embedded or as a standalone daemon (`secantusd-py`).

Single-node only by design: replica sets, sharding, and anything that
depends on real cluster topology are out of scope. Within that
single-node scope, SecantusDB is the database your driver thinks it's
talking to — same handshake, same wire frames, same error codes.

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

# On-disk by default at ./secantus-data; pass storage_path=":memory:" for ephemeral.
with SecantusDBServer(port=27017) as server:
    client = MongoClient(server.uri)
    db = client["mydb"]
    db["users"].insert_one({"_id": 1, "name": "Joe"})
    assert db["users"].find_one({"_id": 1})["name"] == "Joe"
```

## Storage engine

SecantusDB uses **the same WiredTiger C library mongod ships** —
vendored at `vendor/wiredtiger/` (mongodb-7.0.33), built from source
into the wheel, called via WT's official Python SWIG bindings. There
is no Python re-implementation of the storage engine: B-trees, page
eviction, write-ahead logging, durability, on-disk format are all
pure WiredTiger. Your data lives on the same battle-tested engine
mongod uses.

That doesn't make SecantusDB *as fast* as mongod — the layers above
storage (command dispatch, query planner, aggregation pipeline) are
Python, and a like-for-like benchmark currently has SecantusDB
~8×–46× slower per operation than mongod. CRUD reads sit near the
lower end of that; bulk update / delete and aggregation sit at the
upper end where Python loop overhead dominates. See
[`docs/benchmark.md`](https://secantusdb.com/docs/benchmark.html) for current numbers and
methodology. The right use is tests, dev, embedded apps, and
single-node prototypes where conformance + WT durability matter
more than per-op latency.

## What's in scope

Everything a single-node application needs from the wire — the
handshake (`hello` / `isMaster` / `ping` / `buildInfo` / ...), CRUD
(`insert` / `find` / `update` / `delete` / `findAndModify` / `count` /
`drop`), cursors with `getMore` / `killCursors`, aggregation pipelines
and the expression language they need, and **change streams**
(single-node, oplog-backed; collection / db / cluster scope; resume
tokens; `fullDocument: "updateLookup"`; pre-images via
`fullDocumentBeforeChange`; blocking `awaitData` getMore). All backed by
a real query planner with **index acceleration** — single-field,
compound, mixed-direction, partial, TTL, sort — proper `explain` output
(`IXSCAN` vs `COLLSCAN`), and a hash-join `$lookup`.

**Authentication**: SCRAM-SHA-256 — MongoDB's default since 4.0 — is
implemented end-to-end on the wire, alongside **native TLS / mTLS** and
the **MONGODB-X509** cert-as-username mechanism. Off by default; flip
SCRAM on with `secantusd-py --auth` (or `SecantusDBServer(...,
require_auth=True)`), provision users with `createUser`, then connect
with the standard `MongoClient(uri, username=, password=)` shape. See
[Authentication](https://secantusdb.com/docs/authentication.html). Authorization
(RBAC) is *not* enforced — an authenticated principal is currently
treated as fully privileged — and LDAP / Kerberos / GSSAPI / AWS / OIDC
auth mechanisms are out of scope.

What's **out of scope:** real replica sets, sharding, RBAC, auth
mechanisms beyond SCRAM-SHA-256 / MONGODB-X509 (no LDAP / Kerberos /
GSSAPI / AWS / OIDC), `OP_COMPRESSED`, text / hashed / wildcard indexes,
and `$where` / `$function` / `$accumulator` / `mapReduce` (no embedded
JS runtime). If you need those, run a real `mongod`. Native TLS + mTLS,
multi-document transactions (with WiredTiger-native rollback), and geo
support (`$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`,
`$geoNear`, `2dsphere` and `2d` indexes) are all in scope and shipped.

## SQL / PostgreSQL interface (opt-in)

SecantusDB can also speak **SQL over the PostgreSQL wire protocol**. Install the
extra (`pip install "secantus[sql]"`), start a `SecantusPGServer` — optionally
sharing the *same* storage as the MongoDB server — and connect with `psql`,
pg8000, or SQLAlchemy over a `postgresql://` URL:

```python
from secantus.sql import SecantusPGServer

with SecantusPGServer(port=5432) as server:
    ...  # SELECT / INSERT / UPDATE / DELETE, JOIN, GROUP BY, transactions, ...
```

Or run it as a standalone daemon — `pip install "secantus[sql]"` puts a
`secantusd-py-pg` script on your `PATH`:

```bash
secantusd-py-pg --host 127.0.0.1 --port 5432 --storage-path ./secantus-data
```

SQL is compiled down to the same query / aggregation engines the MongoDB side
uses, so it inherits index acceleration and the type system. A collection
written with `pymongo` is queryable as a SQL table with **no `CREATE TABLE`**
(schema-on-read), nested documents surface as `jsonb` (`->`, `->>`, `#>`), and
`BEGIN` / `COMMIT` / `ROLLBACK` are real transactions. Auth (SCRAM-SHA-256) and
TLS work the same as on the Mongo side. See
[SQL / PostgreSQL interface](https://secantusdb.com/docs/sql.html)
for the supported-SQL matrix and examples.

## Installation

```bash
pip install SecantusDB
```

Pre-built wheels are published for CPython **3.10**, **3.11**, **3.12**, and **3.13** on:

- macOS arm64 (Apple Silicon)
- Linux x86_64 and aarch64 (manylinux2014 / glibc, and musllinux_1_2 / Alpine)
- Windows AMD64

macOS Intel (x86_64) is not in the wheel matrix; use a from-source
install if you need it.

WiredTiger is vendored inside the wheel — no separate package, no
compile step, no system build tools required.

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

### The Rust server (separate)

SecantusDB ships **two separate servers** on independent version lines: the
pure-Python server (this package's `SecantusDBServer`) and a self-contained
**Rust server** that speaks the same wire protocol off the GIL. You run one or
the other — there is no in-process engine switching. The Python server is always
pure-Python; the Rust engines live only in the Rust server.

> The old in-process accelerator (`SECANTUS_ENGINE=rust` / `SecantusDBServer(engine=...)`)
> has been **retired** in favour of this two-server split.

The Rust side is a Cargo workspace under `crates/`: a pure-Rust engine crate
(`secantus-core`, no PyO3) reused by the Rust server and the standalone
`secantusd-rs` binary, plus a thin PyO3 bindings crate (`secantus-core-py`)
that builds the `secantus-core` wheel — the vehicle that pins each Rust engine
byte-for-byte against its pure-Python counterpart.

```bash
pip install "secantus[rust]"      # pulls the matching secantus-core wheel
```

The Python server is the **conformance leader** and the default choice: it
passes **99.2%** of pymongo's own test suite. The Rust server runs the same
unmodified suite and currently passes **92.0%** — it's faster per operation
(see [`docs/benchmark.md`](https://secantusdb.com/docs/benchmark.html)) but is
still closing the gap. The features the Rust server doesn't support yet —
`showExpandedEvents` DDL change events, large change-event splitting, read /
write-concern semantics, timeseries `_id` non-uniqueness — and a side-by-side
of when to pick each server are spelled out in
[The two servers](https://secantusdb.com/docs/servers.html).

## Standalone daemon (drop-in `mongod` replacement)

`pip install` puts a `secantusd-py` script on your `PATH`. Run it like
you'd run `mongod`:

```bash
secantusd-py --host 127.0.0.1 --port 27017
# storage at ./secantus-data by default; pass --storage-path :memory:
# for an ephemeral temp dir cleaned up on shutdown.
```

The same `pip install secantus` also puts the standalone **Rust** server on your
`PATH` as `secantusd-rs` (same flags, same wire protocol; see
[The two servers](https://secantusdb.com/docs/servers.html)) —
on Linux, macOS (Apple Silicon), and Windows. Intel-Mac wheels are pure-Python.

Then point any MongoDB driver or tool at it — **no application code
changes**, just the URI:

```bash
mongosh mongodb://127.0.0.1:27017
mongodump --uri mongodb://127.0.0.1:27017 --out ./dump
```

```python
from pymongo import MongoClient
client = MongoClient("mongodb://127.0.0.1:27017")  # same code as for mongod
```

The conformance gauges back this up: the official driver test suites
run **unmodified** against SecantusDB — see the
[conformance validation summary](https://secantusdb.com/docs/validation-summary.html).

## Examples

A walk through the operations a typical application exercises — connect,
insert, index, query, drop. Full version with explanations: [examples in
the docs](https://secantusdb.com/docs/examples.html).

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

# Ephemeral here so the snippet is self-contained; the production default
# is on-disk at ./secantus-data — drop storage_path or set a real path.
with SecantusDBServer(port=0, storage_path=":memory:") as server:
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

## Documentation

Full docs are at [secantusdb.com/docs](https://secantusdb.com/docs/index.html) — with the Rust server's own tree at [secantusdb.com/docs/rust](https://secantusdb.com/docs/rust/index.html).
Highlights:

- [Quickstart](https://secantusdb.com/docs/quickstart.html) — embedding in tests, running standalone.
- [The two servers](https://secantusdb.com/docs/servers.html) — Python vs Rust server, which to use, and what each doesn't support yet.
- [Architecture](https://secantusdb.com/docs/architecture.html) — the layered design.
- [Indexes](https://secantusdb.com/docs/indexes.html) — what `find()` and `aggregate` accelerate,
  `explain` semantics, hints, partial indexes, TTL.
- [Aggregation](https://secantusdb.com/docs/aggregation.html) — supported pipeline stages and
  expression operators.
- [Compatibility](https://secantusdb.com/docs/compatibility.html) — the divergences you should know
  about before you point an application at SecantusDB.
- [Conformance validation](https://secantusdb.com/docs/validation-summary.html) — every
  supported driver's own test suite (pymongo, Go, Node, Java, Ruby,
  Rust, and the PHP library + extension) run **unmodified** against
  SecantusDB, with a cross-driver summary table and a per-driver report
  for each. The other-language gauges catch wire-protocol bugs that
  pymongo's permissive client accepts silently (e.g. int32-vs-int64
  cursor ids).

## Development

```bash
git clone https://github.com/jdrumgoole/SecantusDB.git
cd SecantusDB
uv sync --extra dev
uv run python -m pytest    # 584 tests, runs in parallel under pytest-xdist
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
