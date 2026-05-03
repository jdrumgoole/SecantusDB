# Quickstart

SecantusDB runs in two modes from the same `SecantusDBServer` class:

- **Embedded** — the server runs on a daemon thread inside your Python
  process. Best for tests.
- **Standalone daemon** — a long-running process other tools / processes /
  language runtimes connect to over TCP.

## Embedded (in-process)

The recommended pattern for tests. Each `SecantusDBServer(port=0)` gets its
own OS-assigned port and its own in-memory WiredTiger storage, so multiple
server instances coexist without conflict — pytest-xdist friendly.

```python
import pytest
from pymongo import MongoClient
from secantus import SecantusDBServer


@pytest.fixture
def client():
    with SecantusDBServer(port=0) as server:
        yield MongoClient(server.uri)


def test_insert_and_find(client):
    coll = client["mydb"]["users"]
    coll.insert_many([{"_id": i, "name": f"u{i}"} for i in range(3)])
    assert coll.count_documents({}) == 3
    assert coll.find_one({"_id": 1})["name"] == "u1"
```

The context manager handles startup, shutdown, and cleanup of the temporary
WiredTiger home directory. `server.uri` is a `mongodb://127.0.0.1:<port>/`
string ready for `MongoClient`.

If you don't want a context manager (e.g. a wider pytest fixture), use the
explicit form:

```python
import pytest
from secantus import SecantusDBServer

@pytest.fixture
def server():
    s = SecantusDBServer(port=0)
    s.start()
    try:
        yield s
    finally:
        s.stop()
```

`port=0` is the right default for tests — every fixture instance gets a
fresh OS-assigned port, so `pytest-xdist` workers never collide.

## Standalone daemon

Long-running process; other tools, processes, or language runtimes connect
to it over TCP. Two ways to launch.

### CLI

```bash
python -m secantus --host 127.0.0.1 --port 27117 --log-level INFO
```

Flags:

| Flag | Default | Notes |
|---|---|---|
| `--host` | `127.0.0.1` | bind address |
| `--port` | `27117` | TCP port (vs MongoDB's default 27017, to avoid clashes) |
| `--storage-path` | `:memory:` | WiredTiger home; pass a real directory for persistence |
| `--log-level` | `INFO` | DEBUG / INFO / WARNING / ERROR |

`SIGINT` and `SIGTERM` are handled cleanly — the daemon calls `server.stop()`
in the signal handler so WT shuts down without leaving stale lock files.

```bash
# Persistent daemon — same data across restarts.
python -m secantus --port 27117 --storage-path /var/lib/secantus/cellar
```

### Programmatic

When you want to embed the daemon in a larger long-running app (e.g. a
process supervisor, a sandbox harness):

```python
from secantus import SecantusDBServer

server = SecantusDBServer(
    host="127.0.0.1",
    port=27117,
    storage_path="/var/lib/secantus/cellar",   # persistent WT home
)
server.start()       # returns once the listener is bound
server.wait()        # blocks until server.stop() from another thread / signal
```

`start()` returns immediately. `wait()` blocks until shutdown. A
non-`:memory:` `storage_path` is a real directory: created if missing,
reopened intact across runs (collections, indexes, documents preserved).

## Picking between them

| Use case | Mode |
|---|---|
| Pytest fixture / unit tests | Embedded, `port=0`, `:memory:` |
| Local dev replacement for `mongod` | Daemon (CLI), fixed port |
| CI scratch DB shared across processes | Daemon, persistent `--storage-path` |
| Multi-language test (Python + Node + ...) | Daemon, fixed port |
| Inside a parent application | Programmatic daemon, `start()` + `wait()` |

## Async-friendly

SecantusDB doesn't impose a threading model on client code — the server
runs each connection on its own daemon thread under the hood, so `pymongo`
clients (sync or via `motor`) just work.

## Cleanup

Always use the context-manager form for embedded mode, or call
`server.stop()` in a teardown fixture / signal handler for daemon mode.
The CLI handles this for you. Letting the process exit without stopping
the server is fine for test runs (everything is daemon threads + temp
directories) but emits a warning on graceful shutdown.

## Next

- [Examples](examples.md) — connect, insert, index, query, drop.
- [Architecture](architecture.md) — what's running under the hood.
- [Indexes](indexes.md) — index acceleration semantics, `explain`, hints.
- [Aggregation](aggregation.md) — what pipeline stages and expression
  operators are supported.
- [Compatibility](compatibility.md) — known differences from real `mongod`.
