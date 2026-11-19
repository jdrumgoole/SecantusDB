# Installation

SecantusDB is two servers, `secantusd-mdb` (MongoDB wire protocol) and
`secantusd-pg` (PostgreSQL wire protocol, beta), both written in Rust over
WiredTiger. There are three ways to get them; all three give you the same
binaries.

| Channel | Gives you | Needs |
| --- | --- | --- |
| [PyPI](#from-pypi) — `pip install SecantusDB` | both servers, prebuilt, plus the `secantusdb` Python launcher | Python 3.10+; Linux x86_64 or macOS arm64 |
| [crates.io](#from-cratesio) — `cargo install` | each server built from source, and each as a Rust library | a Rust toolchain, CMake, a C compiler |
| [GitHub Releases](#prebuilt-binaries) | prebuilt archives of each server | nothing |

## From PyPI

```bash
pip install SecantusDB
```

The wheel carries the two Rust servers as prebuilt binaries and a small
Python launcher. It needs Python 3.10 or newer and is published for:

- Linux x86_64 (glibc; the wheel tag names the minimum glibc version)
- macOS arm64 (Apple Silicon)

| command | server |
| --- | --- |
| `secantusd-mdb` | MongoDB wire |
| `secantusd-pg` | PostgreSQL wire (beta) |

Both commands go on your `PATH`, so you can run either as a daemon:

```bash
secantusd-mdb --port 27017 --storage-path ./secantus-data
secantusd-pg ./secantus-pg-data 127.0.0.1:5432
```

From Python, the `secantusdb` package starts either server on a free port with
a temporary data directory:

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
(`secantusd-pg --version` prints it). There is no sdist.

## From crates.io

Each server is a crate that installs a daemon and is also a Rust library:

| Crate | Installs | Library |
| --- | --- | --- |
| [`secantus-mdb`](https://crates.io/crates/secantus-mdb) | `secantusd-mdb` | `secantus_mdb::Server` |
| [`secantus-pg`](https://crates.io/crates/secantus-pg) | `secantusd-pg` | `secantus_pg::PgServer` |

```bash
cargo install secantus-mdb --version 0.5.3-beta.198   # MongoDB: the `secantusd-mdb` daemon
cargo install secantus-pg  --version 0.1.0-beta.20   # PostgreSQL: the `secantusd-pg` daemon
secantusd-mdb --version
```

Both crates are pre-releases, so `cargo install` needs `--version`: without it
cargo stops at "nothing to install". Take the current version from each
crate's page on crates.io.

Building compiles WiredTiger (and, for `secantus-pg`, libpg_query) from
source, which needs a Rust toolchain (the crates declare `rust-version =
"1.98"`), **CMake and a C compiler**; `secantus-pg` also needs **libclang**.
The first build takes a few minutes; `cargo install` uses link-time
optimisation, so budget about ten for the PostgreSQL server. If you only want
the binary, `cargo binstall secantus-mdb` / `cargo binstall secantus-pg` fetch
the prebuilt one from GitHub Releases where one exists for your platform and
build from source otherwise.

To run a server inside a Rust program, add the crate as a dependency and start
it in-process:

```toml
[dependencies]
secantus-mdb = "0.5.3-beta.198"     # or secantus-pg = { version = "0.1.0-beta.19", default-features = false }
```

```rust
let server = secantus_mdb::Server::start()?;   // temporary store, OS-assigned port
let uri = server.uri();                        // hand this to the mongodb driver
```

`Server::builder()` sets a persistent store, a port, authentication, TLS and
the WiredTiger cache; `secantus_pg::PgServer` has the same shape (`start()`,
`builder()`, `dsn()`, `url()`). See
[Embedded in your application](https://secantusdb.com/docs/rust/embedded.html)
and the crate READMEs.

## Prebuilt binaries

Standalone archives, with WiredTiger statically linked and no other runtime
dependency, are attached to the releases on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases):

| Server | Release tag | Archive | Platforms |
| --- | --- | --- | --- |
| MongoDB | `secantusdb-v<version>` | `secantusdb-<version>-<target>.tar.gz` | Linux x86_64, macOS arm64, Windows x86_64 (a `.zip` too from the release after 0.5.3-beta.197) |
| PostgreSQL | `secantusd-pg-v<version>` | `secantusd-pg-<version>-<target>.tar.gz` | Linux x86_64, macOS arm64 |

`<target>` is the Rust target triple: `x86_64-unknown-linux-gnu`,
`aarch64-apple-darwin` or `x86_64-pc-windows-msvc`. Each archive has a
`.sha256` file beside it. Download, verify, extract, run:

```bash
tag="secantusdb-v<version>"
base="https://github.com/jdrumgoole/SecantusDB/releases/download/$tag"
curl -LO "$base/secantusdb-<version>-x86_64-unknown-linux-gnu.tar.gz"
curl -LO "$base/secantusdb-<version>-x86_64-unknown-linux-gnu.tar.gz.sha256"
shasum -a 256 -c secantusdb-<version>-x86_64-unknown-linux-gnu.tar.gz.sha256
tar xzf secantusdb-<version>-x86_64-unknown-linux-gnu.tar.gz
./secantusdb-<version>-x86_64-unknown-linux-gnu/secantusd-mdb --version
```

The Rust server docs have the full walk-through, including Windows
([installation](https://secantusdb.com/docs/rust/installation.html)).

On a platform none of these cover (Linux on arm64, say), build from crates.io.

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
build cibuildwheel runs in CI), so `cmake` / `ninja` / `swig` / a C/C++
compiler must be on `PATH`.

Common workflows:

```bash
uv run python -m invoke fmt    # ruff format
uv run python -m invoke lint   # ruff check
uv run python -m invoke test   # pytest, parallel
uv run python -m invoke docs   # build Sphinx docs (warnings as errors)
```

The test suite runs in parallel via `pytest-xdist`. Tests use `port=0`
and `:memory:` storage so they don't share state across workers.
