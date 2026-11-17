# Quickstart

SecantusDB is two single-node database servers, each a real server process
speaking a real wire protocol over WiredTiger:

| Server | Command | Wire protocol | Clients |
| --- | --- | --- | --- |
| MongoDB server | `secantusd-mdb` | MongoDB | `pymongo`, `mongosh`, every MongoDB driver |
| PostgreSQL server (beta) | `secantusd-pg` | PostgreSQL | `psycopg`, `psql`, SQLAlchemy, every PostgreSQL driver |

[Install](installation.md) them from PyPI, crates.io, or a prebuilt archive,
then run one as a daemon or start it from your own code. Both keep data on
disk, in the directory you give them.

## The MongoDB server

```bash
secantusd-mdb --host 127.0.0.1 --port 27017 --storage-path ./secantus-data
```

It prints `secantusd-mdb listening on <host>:<port>` once it accepts
connections, runs until `SIGINT` / `SIGTERM`, and shuts WiredTiger down
cleanly. `./secantus-data` is created on first run and reopened intact on
later runs. Then, from another shell, use the same tools you would use against
`mongod`:

```bash
mongosh mongodb://127.0.0.1:27017
mongodump    --uri mongodb://127.0.0.1:27017 --out ./dump
mongorestore --uri mongodb://127.0.0.1:27017 ./dump
```

```python
from pymongo import MongoClient

client = MongoClient("mongodb://127.0.0.1:27017")
bottles = client["wine_cellar"]["bottles"]
bottles.insert_many(
    [
        {"_id": 1, "name": "Pommard 2018", "year": 2018},
        {"_id": 2, "name": "Brunello 2015", "year": 2015},
        {"_id": 3, "name": "Barolo 2017", "year": 2017},
    ]
)
bottles.create_index([("year", 1)])
older = list(bottles.find({"year": {"$lte": 2017}}).sort("year"))
print([b["name"] for b in older])  # ['Brunello 2015', 'Barolo 2017']
```

Any other driver works the same way; `mongo-go-driver`, for instance (see the
[Go-driver validation report](validation-report-go-rust-server.md)):

```go
client, _ := mongo.Connect(options.Client().ApplyURI("mongodb://127.0.0.1:27017"))
```

Common flags (the full table, the `secantusd.toml` config file, TLS and
authentication are in the [Rust server docs](https://secantusdb.com/docs/rust/running.html)
and [Configuration](configuration.md)):

| Flag | Default | Notes |
| --- | --- | --- |
| `--host` | `127.0.0.1` | bind address |
| `--port` | `27017` | TCP port; `0` picks a free one |
| `--storage-path` | `./secantus-data` | WiredTiger home directory |
| `--auth` | off | require SCRAM authentication |
| `--log-level` | `INFO` | `DEBUG` / `INFO` / `WARNING` / `ERROR` |

## The PostgreSQL server (beta)

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432   # storage path, bind address
```

```python
import psycopg

with psycopg.connect("postgresql://postgres@127.0.0.1:5432/postgres", autocommit=True) as conn:
    conn.execute("CREATE TABLE users (id int PRIMARY KEY, name text)")
    conn.execute("INSERT INTO users VALUES (1, 'Joe')")
    print(conn.execute("SELECT name FROM users WHERE id = 1").fetchone())  # ('Joe',)
```

`psql`, SQLAlchemy and the other PostgreSQL drivers connect the same way. See
[SQL / PostgreSQL interface](sql.md) for the SQL surface.

## Starting a server from your own code

Each server can be started from the program that uses it, on a free port, with
its data in a temporary directory removed on stop (or in a directory you pass
as `storage_path`).

**From Python**, the `secantusdb` launcher that `pip install SecantusDB`
provides starts the real server binary as a subprocess:

```python
from pymongo import MongoClient
from secantusdb import MongoServer, PostgresServer

with MongoServer() as server:                      # a free port, a temp directory
    client = MongoClient(server.uri)
    client["mydb"]["users"].insert_one({"_id": 1, "name": "Joe"})

with PostgresServer(storage_path="./pg-data", port=5432) as server:
    print(server.dsn)                              # postgresql://postgres@127.0.0.1:5432/postgres
```

**From Rust**, the crates are libraries as well: `secantus_mdb::Server` and
`secantus_pg::PgServer` run the server in-process (see
[Embedded in your application](https://secantusdb.com/docs/rust/embedded.html)).

```rust
let server = secantus_mdb::Server::start()?;        // temporary store, OS-assigned port
let client = mongodb::sync::Client::with_uri_str(server.uri())?;
```

A scratch instance on a free port with a temporary store is also what a test
suite wants: start one per test or per session and point the driver at it.

## Next

- [Installation](installation.md) — PyPI, crates.io and prebuilt binaries.
- [Examples](examples.md) — connect, insert, index, query, drop.
- [The servers](servers.md) — the two Rust servers and what each supports.
- [Architecture](architecture.md) — what's running under the hood.
- [Indexes](indexes.md) — index acceleration semantics, `explain`, hints.
- [Aggregation](aggregation.md) — what pipeline stages and expression
  operators are supported.
- [Compatibility](compatibility.md) — known differences from real `mongod`.
- [Deprecated: Python servers](python-servers.md) — the original Python
  servers, kept as a record.
