# SecantusDB

[![Status: alpha](https://img.shields.io/badge/status-alpha-orange)](#alpha-software)
[![Tests: 584 passing](https://img.shields.io/badge/tests-584%20passing-brightgreen)](#)
[![License: GPL-2.0-only (code) + CC-BY-4.0 (content)](https://img.shields.io/badge/license-GPL--2.0--only%20%2B%20CC--BY--4.0-blue)](#license)
[![Python: 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)](https://www.python.org/)
[![Documentation Status](https://readthedocs.org/projects/secantusdb/badge/?version=latest)](https://secantusdb.readthedocs.io/en/latest/)

> [!WARNING]
> **Alpha software.** <a id="alpha-software"></a>
>
> SecantusDB is in early development. The Python API surface (CLI flags,
> public class signatures) is still settling and may shift between point
> releases. **The on-disk format is WiredTiger's** — the same engine
> MongoDB uses — and the schema we layer on top (collection / index /
> oplog tables) has been stable across releases; the test suite runs
> against real on-disk WiredTiger storage and the
> [persistence tests](tests/test_storage.py) explicitly verify
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
embedded or as a standalone daemon (`secantusdb`).

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
[`docs/benchmark.md`](docs/benchmark.md) for current numbers and
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
implemented end-to-end on the wire. Off by default; flip on with
`secantusdb --auth` (or `SecantusDBServer(..., require_auth=True)`),
provision users with `createUser`, then connect with the standard
`MongoClient(uri, username=, password=)` shape. See
[Authentication](docs/authentication.md). Authorization (RBAC), x509,
LDAP, Kerberos, and TLS are *not* implemented — an authenticated
principal is currently treated as fully privileged.

What's **out of scope:** real replica sets, sharding, RBAC, x509 /
LDAP / Kerberos auth, TLS, text / geo / wildcard indexes, `$where`,
real transaction rollback. If you need those, run a real `mongod`.

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

See [Installation](docs/installation.md) for dev-install instructions.

## Standalone daemon (drop-in `mongod` replacement)

`pip install` puts a `secantusdb` script on your `PATH`. Run it like
you'd run `mongod`:

```bash
secantusdb --host 127.0.0.1 --port 27017
# storage at ./secantus-data by default; pass --storage-path :memory:
# for an ephemeral temp dir cleaned up on shutdown.
```

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

The conformance gauges back this up: pymongo's own test suite and
mongo-go-driver's own test suite run **unmodified** against SecantusDB —
see [pymongo validation report](docs/validation-report.md) and
[Go-driver validation report](docs/validation-report-go.md).

## Examples

A walk through the operations a typical application exercises — connect,
insert, index, query, drop. Full version with explanations: [examples in
the docs](https://secantusdb.readthedocs.io/en/latest/examples.html).

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

Full docs are in `docs/`; build them with `uv run python -m invoke docs`
and open `docs/_build/html/index.html`. Highlights:

- [Quickstart](docs/quickstart.md) — embedding in tests, running standalone.
- [Architecture](docs/architecture.md) — the layered design.
- [Indexes](docs/indexes.md) — what `find()` and `aggregate` accelerate,
  `explain` semantics, hints, partial indexes, TTL.
- [Aggregation](docs/aggregation.md) — supported pipeline stages and
  expression operators.
- [Compatibility](docs/compatibility.md) — the divergences you should know
  about before you point an application at SecantusDB.
- [pymongo validation report](docs/validation-report.md) — per-category
  pass / fail / skip rate from running **pymongo's own test suite,
  unmodified**, against SecantusDB. The submodule at
  `vendor/pymongo-tests/` is checked out at the pinned upstream tag with
  zero local edits; a pytest plugin starts an embedded server and points
  pymongo's `DB_IP`/`DB_PORT` at it.
- [Go-driver validation report](docs/validation-report-go.md) —
  same shape against **mongo-go-driver's own test suite, unmodified**.
  Spawns a standalone SecantusDB daemon and runs `go test` with
  `MONGODB_URI` pointed at it. The Go driver underpins `mongodump` /
  `mongorestore` and most non-Python tooling, so this gauge catches
  type-strict wire bugs (int32 vs int64) that pymongo accepts silently.
- [Node-driver validation report](docs/validation-report-node.md) —
  same shape against **mongo-node-driver's own test suite, unmodified**.
  Spawns a standalone SecantusDB daemon and runs mocha with
  `MONGODB_URI` pointed at it. Initial baseline is restricted to
  the import-clean subset of unit tests because of an unrelated
  ESM/TypeScript loader quirk in node-mongodb-native v7.2.0; see
  `node_validation/include_paths.py` for the rationale.
- [Java-driver validation report](docs/validation-report-java.md) —
  same shape against **mongo-java-driver's own test suite, unmodified**.
  Spawns a standalone SecantusDB daemon and invokes the driver's
  bundled `./gradlew` with `-Dorg.mongodb.test.uri=mongodb://...`.
  Initial baseline is the `:bson:test` module (BSON serialization,
  ~289 test files); the JDBC-style integration modules can be added
  to `java_validation/include_modules.py` as we widen.

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

- **Code** — GPL-2.0-only. See [`LICENSE`](LICENSE). SecantusDB bundles
  the [WiredTiger](https://github.com/wiredtiger/wiredtiger) storage
  engine (itself GPL-2/GPL-3), so the combined work is GPL.
- **Written content** — [Creative Commons Attribution 4.0
  International (CC-BY 4.0)](https://creativecommons.org/licenses/by/4.0/).
  See [`LICENSE-DOCS`](LICENSE-DOCS). Covers `README.md`, everything
  under `docs/`, the validation reports, and `pymongo_validation/README.md`.
  Operational instructions to AI assistants (`CLAUDE.md`) and vendored
  third-party content (under `vendor/`) are out of scope.
