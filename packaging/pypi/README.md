# SecantusDB

SecantusDB is a pair of single-node database servers, one speaking the
MongoDB wire protocol and one speaking the PostgreSQL wire protocol (beta),
both over the WiredTiger storage engine MongoDB ships. Any MongoDB or
PostgreSQL driver connects to it as it would to `mongod` or `postgres`.

This package bundles both servers, prebuilt, for Linux x86_64 and macOS
arm64. The MongoDB server also runs inside your Python process:

| command | server | version |
| --- | --- | --- |
| `secantusd-mdb` | MongoDB wire | {mongodb_version} |
| `secantusd-pg` | PostgreSQL wire (beta) | {postgres_version} |

## Start a server from Python

```python
import pymongo
from secantusdb import MongoServer

with MongoServer() as server:
    client = pymongo.MongoClient(server.uri)
    client.test.things.insert_one({"x": 1})
```

`MongoServer()` runs the server **in this process**, as SQLite does: no
child process, but a real loopback TCP port that any MongoDB driver
connects to. It advertises a single-node replica set, so change streams and
transactions work. A crash in the server ends your process too;
`MongoServer(embedded=False)` runs the `secantusd-mdb` binary as a child
process instead. A server started in-process cannot be stopped from a
`fork()`ed child; use `multiprocessing`'s `spawn` start method.

On a Python without the stable ABI (free-threaded CPython), pip installs a
wheel with no in-process server: there `MongoServer()` raises, naming
`embedded=False`.

```python
import psycopg
from secantusdb import PostgresServer

with PostgresServer() as server:
    with psycopg.connect(server.dsn) as conn:
        print(conn.execute("select 1").fetchone())
```

Each server gets a free port and a temporary data directory, removed when
the `with` block ends. Pass `storage_path=` to keep the data, or `port=` to
pick the port. The PostgreSQL server runs as a child process.

## Run a server from the shell

```
secantusd-mdb --port 27018 --storage-path ./data
secantusd-pg ./pgdata 127.0.0.1:5433
```

Licence: GPL-2.0-only. Documentation: https://secantusdb.com
