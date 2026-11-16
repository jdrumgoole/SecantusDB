# Installation

```bash
pip install SecantusDB
```

The wheel carries the two Rust servers as prebuilt binaries and a small
Python launcher. It needs Python 3.10 or newer and is published for:

- Linux x86_64 (glibc; the wheel tag names the minimum glibc version)
- macOS arm64 (Apple Silicon)

| command | server |
| --- | --- |
| `secantusd-rs` | MongoDB wire |
| `secantusd-pg` | PostgreSQL wire (beta) |

Both commands go on your `PATH`. From Python, the `secantusdb` package
starts either server on a free port with a temporary data directory:

```python
from secantusdb import MongoServer, PostgresServer

with MongoServer() as mongo:        # mongo.uri  -> pymongo.MongoClient(...)
    ...
with PostgresServer() as pg:        # pg.dsn     -> psycopg.connect(...)
    ...
```

Pass `storage_path=` to keep the data after the server stops, and `port=` to
choose the port. The wheel's version is the MongoDB server's; the PostgreSQL
server in it is the newest `secantusd-pg` release when the wheel was built
(`secantusd-pg --version` prints it).

The wheel does not carry the deprecated Python servers.

## Other platforms

There is no sdist: on a platform the wheel does not cover, install the
servers from crates.io (`cargo install secantus-mdb --version <v>`,
`cargo install secantus-pg --version <v>`; each needs a Rust toolchain and
builds WiredTiger as part of the crate), or unpack a standalone archive from
the `secantusdb-v*` / `secantusd-pg-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases)
(`secantusd-rs` is also built for Windows x86_64).

## Development install

For working on SecantusDB itself, clone the repo and use `uv`:

```bash
git clone https://github.com/jdrumgoole/SecantusDB.git
cd SecantusDB
git submodule update --init --recursive   # vendor/wiredtiger
uv sync --extra dev                       # also builds vendored WT
uv run python -m pytest                   # full parallel suite
```

A development clone always builds WiredTiger from source (the same
build cibuildwheel runs in CI), so the prerequisites in the previous
section apply: `cmake` / `ninja` / `swig` / a C/C++ compiler must be
on `PATH`.

Common workflows:

```bash
uv run python -m invoke fmt    # ruff format
uv run python -m invoke lint   # ruff check
uv run python -m invoke test   # pytest, parallel
uv run python -m invoke docs   # build Sphinx docs (warnings as errors)
```

The test suite runs in parallel via `pytest-xdist`. Tests use `port=0`
and `:memory:` storage so they don't share state across workers.
