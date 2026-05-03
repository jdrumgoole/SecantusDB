# SecantusDB

A fake MongoDB server in Python. SecantusDB speaks the subset of the MongoDB
wire protocol that [`pymongo`](https://pymongo.readthedocs.io/en/stable/) emits,
so test suites can talk to it instead of starting a real `mongod`. Replica sets,
sharding, and cluster-only features are out of scope.

## Installation

```bash
pip install secantus
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

If your platform isn't in the matrix above, `pip install secantus`
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

## Quick start

Run a standalone server on a fixed port:

```bash
uv run python -m secantus --host 127.0.0.1 --port 27117
```

Or embed one in a pytest fixture:

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as server:
    client = MongoClient(server.uri)
    # ... run pymongo calls against secantus ...
```

The `port=0` form lets the OS pick a free port, which is what tests should
do so they can run in parallel (`pytest-xdist`-friendly).

## Documentation

Full Sphinx docs live in `docs/`; build them with `uv run python -m invoke docs`.

## License

SecantusDB is licensed under **GPL-2.0-only**. See [`LICENSE`](LICENSE).

The license is GPL because SecantusDB depends on (and intends to bundle) the
[WiredTiger](https://github.com/wiredtiger/wiredtiger) storage engine, which is
itself GPL-2/GPL-3. Bundling GPL code requires a GPL-compatible license on the
combined work; GPL-2-only is the closest match to WiredTiger's primary license.

If you need other terms, contact the author for a commercial arrangement.
