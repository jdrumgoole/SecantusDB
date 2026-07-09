# Quickstart

SecantusDB runs in two modes from the same `SecantusDBServer` class:

- **Embedded** — the server runs on a daemon thread inside your Python
  process. Best when your application owns the database lifetime.
- **Standalone daemon** — a long-running process that other tools,
  processes, or language runtimes connect to over TCP. **This is the
  drop-in `mongod` replacement**: any standard MongoDB driver or
  command-line tool (`pymongo`, `mongo-go-driver`, `mongosh`,
  `mongodump`, `mongorestore`, ...) connects unchanged — just point
  the URI at SecantusDB's host:port.

Both modes default to **on-disk storage** at `./secantus-data` so data
survives restarts. Pass `storage_path=":memory:"` (or `--storage-path
:memory:` on the CLI) for an ephemeral, test-friendly mode.

## Embedded (in-process)

A real script — start the server, connect, do work, shut down. The
context manager handles startup and shutdown; `./secantus-data` is
created on first run and reopened intact on later runs.

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=27017) as server:
    client = MongoClient(server.uri)
    bottles = client["wine_cellar"]["bottles"]

    bottles.insert_one({"_id": 1, "name": "Pommard 2018", "year": 2018})
    bottles.insert_many(
        [
            {"_id": 2, "name": "Brunello 2015", "year": 2015},
            {"_id": 3, "name": "Barolo 2017", "year": 2017},
        ]
    )

    bottles.create_index([("year", 1)])
    older = list(bottles.find({"year": {"$lte": 2017}}).sort("year"))
    print([b["name"] for b in older])
    # ['Brunello 2015', 'Barolo 2017']
```

When the `with` block exits the server stops cleanly. `./secantus-data`
keeps the inserted documents — re-run the same script and `bottles.find`
sees them again.

For a different on-disk location, pass `storage_path=`:

```python
with SecantusDBServer(port=27017, storage_path="/var/lib/cellar") as server:
    ...
```

## Standalone daemon

Long-running process; other tools, processes, or language runtimes
connect to it over TCP. Two ways to launch.

### CLI

```bash
secantusd-py --host 127.0.0.1 --port 27017
# storage at ./secantus-data by default
```

`secantusd-py` is the standalone single-node Python server installed by `pip
install SecantusDB`. The legacy aliases `secantusdb` / `secantus` and the module
form `python -m secantus` invoke the same entry point.

Then from another shell — same commands you'd run against `mongod`:

```bash
mongosh mongodb://127.0.0.1:27017
mongodump   --uri mongodb://127.0.0.1:27017 --out ./dump
mongorestore --uri mongodb://127.0.0.1:27017 ./dump
```

Or from a Python script connecting to the running daemon:

```python
from pymongo import MongoClient

client = MongoClient("mongodb://127.0.0.1:27017")
client["wine_cellar"]["bottles"].insert_one({"_id": 1, "name": "Pommard 2018"})
```

Or from Go — `mongo-go-driver` works the same way (see the
[Go-driver validation report](validation-report-go.md)):

```go
client, _ := mongo.Connect(options.Client().ApplyURI("mongodb://127.0.0.1:27017"))
```

CLI flags:

| Flag | Default | Notes |
|---|---|---|
| `--host` | `127.0.0.1` | bind address |
| `--port` | `27017` | TCP port (matches MongoDB's standard so existing tools just work) |
| `--storage-path` | `./secantus-data` | WiredTiger home; pass `:memory:` for ephemeral |
| `--log-level` | `INFO` | DEBUG / INFO / WARNING / ERROR |

`SIGINT` and `SIGTERM` are handled cleanly — the daemon calls
`server.stop()` in the signal handler so WiredTiger shuts down without
leaving stale lock files.

### Programmatic

When you embed the daemon in a larger long-running app (process
supervisor, sandbox harness, etc.):

```python
from secantus import SecantusDBServer

server = SecantusDBServer(host="127.0.0.1", port=27017, storage_path="/var/lib/cellar")
server.start()       # returns once the listener is bound
server.wait()        # blocks until server.stop() from another thread / signal
```

## Ephemeral / test mode

For tests or scratch experiments, set `storage_path=":memory:"`. The
server uses a tempdir WiredTiger home that's wiped on `stop()` — no
files left behind, no port conflicts, fully parallel-safe. Combine with
`port=0` so the OS picks a free port:

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0, storage_path=":memory:") as server:
    client = MongoClient(server.uri)
    # ... use client; nothing persists past this block ...
```

Pytest fixture form:

```python
import pytest
from pymongo import MongoClient
from secantus import SecantusDBServer

@pytest.fixture
def client():
    with SecantusDBServer(port=0, storage_path=":memory:") as server:
        yield MongoClient(server.uri)
```

## Picking between them

| Use case | Mode |
|---|---|
| Application owns the DB lifetime | Embedded, on-disk default |
| Local dev replacement for `mongod` | Daemon (CLI), on-disk default |
| CI scratch DB shared across processes | Daemon (CLI), persistent path |
| Multi-language test (Python + Node + ...) | Daemon (CLI), fixed port |
| Inside a parent application | Programmatic daemon, `start()` + `wait()` |
| Pytest fixture / unit tests | Embedded, `port=0`, `:memory:` |

## Async-friendly

SecantusDB doesn't impose a threading model on client code — the server
runs each connection on its own daemon thread under the hood, so
`pymongo` clients (sync or via `motor`) just work.

## Cleanup

Always use the context-manager form for embedded mode, or call
`server.stop()` in a teardown / signal handler for daemon mode. The CLI
handles this for you. Letting the process exit without stopping the
server is fine for the `:memory:` mode (everything is daemon threads +
temp directories) but a persistent on-disk run will leave WiredTiger
lock files behind that recovery has to clear on next open.

## Next

- [Examples](examples.md) — connect, insert, index, query, drop.
- [Architecture](architecture.md) — what's running under the hood.
- [Indexes](indexes.md) — index acceleration semantics, `explain`, hints.
- [Aggregation](aggregation.md) — what pipeline stages and expression
  operators are supported.
- [Compatibility](compatibility.md) — known differences from real `mongod`.
