# SecantusDB

[![Status: alpha](https://img.shields.io/badge/status-alpha-orange)](#alpha-software)
[![Tests: 584 passing](https://img.shields.io/badge/tests-584%20passing-brightgreen)](#)
[![License: GPL-2.0-only](https://img.shields.io/badge/license-GPL--2.0--only-blue)](LICENSE)
[![Python: 3.12+](https://img.shields.io/badge/python-3.12%2B-blue)](https://www.python.org/)
[![Documentation Status](https://readthedocs.org/projects/secantusdb/badge/?version=latest)](https://secantusdb.readthedocs.io/en/latest/)

> [!WARNING]
> **Alpha software — not for production use.** <a id="alpha-software"></a>
>
> SecantusDB is in early development. The API surface and on-disk format
> can change between point releases. It exists today as a developer tool
> for exercising application code against a MongoDB-shaped surrogate; it
> is **not** a database for storing data you can't recreate.

A **surrogate single-node MongoDB server** in Python. SecantusDB speaks the
subset of the MongoDB wire protocol that
[`pymongo`](https://pymongo.readthedocs.io/en/stable/) emits, so test suites
can talk to it **instead of** standing up a real `mongod`. No binary to
install, no port conflicts, parallel-test friendly. Single-node only —
replica sets and sharding are out of scope by design.

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

# On-disk by default at ./secantus-data; pass storage_path=":memory:" for ephemeral.
with SecantusDBServer(port=27117) as server:
    client = MongoClient(server.uri)
    db = client["mydb"]
    db["users"].insert_one({"_id": 1, "name": "Joe"})
    assert db["users"].find_one({"_id": 1})["name"] == "Joe"
```

## What's in scope

The subset of MongoDB that `pymongo` actually drives — connection handshake,
CRUD, cursors, aggregation, `findAndModify` — backed by a real query
planner with **index acceleration** (single-field, compound,
mixed-direction, partial, TTL, sort), proper `explain` output (`IXSCAN`
vs `COLLSCAN`), and a hash-join `$lookup`.

What's **out of scope:** replica sets, sharding, change streams,
authentication, TLS, text/geo/wildcard indexes, `$where`, real transaction
rollback. If your test depends on those, run a real `mongod`.

## Installation

```bash
pip install SecantusDB
```

Pre-built wheels are published for CPython **3.12** and **3.13** on:

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

## Examples

A walk through the operations a typical test exercises — connect, insert,
index, query, drop. Full version with explanations: [examples in the
docs](https://secantusdb.readthedocs.io/en/latest/examples.html).

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
  about before you trust SecantusDB for a given test.

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

GPL-2.0-only. See [`LICENSE`](LICENSE). SecantusDB intends to bundle the
[WiredTiger](https://github.com/wiredtiger/wiredtiger) storage engine
(itself GPL-2/GPL-3), so the combined work is GPL.
