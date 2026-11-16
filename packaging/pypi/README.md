# SecantusDB

SecantusDB is a pair of single-node database servers, one speaking the
MongoDB wire protocol and one speaking the PostgreSQL wire protocol (beta),
both over the WiredTiger storage engine MongoDB ships. Point your tests at it
instead of standing up a real `mongod` or `postgres`.

This wheel bundles both servers as prebuilt binaries for Linux x86_64 and
macOS arm64:

| command | server | version |
| --- | --- | --- |
| `secantusd-mdb` | MongoDB wire | {mongodb_version} |
| `secantusd-pg` | PostgreSQL wire (beta) | {postgres_version} |

## Start a server in a test

```python
import pymongo
from secantusdb import MongoServer

with MongoServer() as server:
    client = pymongo.MongoClient(server.uri)
    client.test.things.insert_one({"x": 1})
```

```python
import psycopg
from secantusdb import PostgresServer

with PostgresServer() as server:
    with psycopg.connect(server.dsn) as conn:
        print(conn.execute("select 1").fetchone())
```

Each server gets a free port and a temporary data directory, removed when
the `with` block ends. Pass `storage_path=` to keep the data, or `port=` to
pick the port.

## Run a server from the shell

```
secantusd-mdb --port 27018 --storage-path ./data
secantusd-pg ./pgdata 127.0.0.1:5433
```

Licence: GPL-2.0-only. Documentation: https://secantusdb.com
