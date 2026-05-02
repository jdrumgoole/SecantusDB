# SecantusDB

A fake MongoDB server in Python. SecantusDB speaks the subset of the MongoDB
wire protocol that [`pymongo`](https://pymongo.readthedocs.io/en/stable/) emits,
so test suites can talk to it instead of starting a real `mongod`. Replica sets,
sharding, and cluster-only features are out of scope.

## Installation

`secantus` depends on the third-party
[`wiredtiger`](https://pypi.org/project/wiredtiger/) PyPI package, which
currently ships only as a source distribution. Until binary wheels are
available, `pip install secantus` triggers a from-source build of WiredTiger
that needs three native build tools on `PATH`:

- **`cmake`** (>= 3.21)
- **`ninja`**
- **`swig`** (>= 4.0)

Install the prerequisites first, then `secantus`.

### macOS

```bash
brew install cmake ninja swig
pip install secantus
```

If you use `uv`-managed Python, prefer:

```bash
uv tool install cmake
uv tool install ninja
brew install swig
uv pip install secantus
```

### Linux (Debian/Ubuntu)

```bash
sudo apt-get install -y cmake ninja-build swig
pip install secantus
```

### Linux (Fedora/RHEL)

```bash
sudo dnf install -y cmake ninja-build swig
pip install secantus
```

### Windows

WiredTiger's Python bindings have not been validated on Windows by the
SecantusDB project. macOS and Linux are the supported development targets.

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
