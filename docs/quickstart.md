# Quickstart

Two ways to use SecantusDB: embedded in a process (typical for tests), or as
a standalone server on a fixed port.

## Embedded in tests

The recommended pattern. Each `SecantusDBServer(port=0)` gets its own
OS-assigned port and its own in-memory WiredTiger storage, so multiple
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

The context manager handles startup, shutdown, and cleanup of the
temporary WiredTiger home directory.

`server.uri` is a `mongodb://127.0.0.1:<port>` string ready for
`MongoClient`.

## Standalone server

Useful when you want to point an existing tool at SecantusDB:

```bash
uv run python -m secantus --host 127.0.0.1 --port 27117
```

Or in code:

```python
from secantus import SecantusDBServer

server = SecantusDBServer(host="127.0.0.1", port=27117)
server.start()
try:
    # ... server is now accepting connections ...
    pass
finally:
    server.stop()
```

By default, storage is in-memory (`:memory:`). To persist between runs,
pass an explicit path:

```python
SecantusDBServer(host="127.0.0.1", port=27117, db_home="/tmp/secantus-data")
```

The directory will be created if it doesn't exist; reopening it later
preserves all collections, indexes, and documents.

## Async-friendly tests

SecantusDB doesn't impose any threading model on test code — the server
runs each connection on its own daemon thread under the hood, so `pymongo`
clients (sync or via `motor`) just work.

## Cleanup

Always use the context-manager form, or call `server.stop()` in a teardown
fixture. Letting the process exit without stopping the server is fine for
test runs (everything is daemon threads + temp directories) but produces a
warning on graceful shutdown.

## Next

- [Architecture](architecture.md) — what's running under the hood.
- [Indexes](indexes.md) — index acceleration semantics, `explain`, hints.
- [Aggregation](aggregation.md) — what pipeline stages and expression
  operators are supported.
- [Compatibility](compatibility.md) — known differences from real `mongod`.
