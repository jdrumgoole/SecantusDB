# Embedded in your application

## From Rust

The `secantus-mdb` crate starts the server **in-process**: a real server on a
real TCP port, with a temporary store removed on drop, so any number of servers
can run in parallel. Add it as a dependency and connect the official
`mongodb` driver:

```rust
let server = secantus_mdb::Server::start()?;   // temporary store, OS-assigned port
let client = mongodb::sync::Client::with_uri_str(server.uri())?;
client.database("mydb").collection("users")
    .insert_one(mongodb::bson::doc! {"_id": 1, "name": "Joe"})?;
```

`Server::builder()` sets a persistent store, a port, auth, TLS, the WiredTiger
cache and the background sweepers. Like the daemon, it expires TTL-indexed
documents every 60 seconds. To get an expiry sooner, set
`ttlMonitorSleepSecs` with `setParameter`, as on `mongod`, and wait for `serverStatus().metrics.ttl.passes` to move instead of sleeping. A
server started this way advertises a
single-node replica set named `secantus` and enables test commands.

## From Python

`pip install SecantusDB` provides the `secantusdb` package. Its
`MongoServer` runs the MongoDB server **inside your Python process**, the way
SQLite does, on a free port and with a temporary data directory unless you
pass `storage_path`:

```python
from pymongo import MongoClient
from secantusdb import MongoServer

with MongoServer() as server:
    client = MongoClient(server.uri)
    client["mydb"]["users"].insert_one({"_id": 1, "name": "Joe"})
```

"In-process" means no child process, not no socket. The server's threads
serve a real loopback TCP port, and the driver connects to `server.uri` as it
would to `mongod`; Python starts and stops the server and is never in the
request path. The server behaves as the `secantusd-mdb` daemon does by
default: it advertises the single-node `secantus` replica set (so change
streams and transactions work), test commands are off, and TTL-indexed
documents expire every 60 seconds.

`MongoServer(embedded=False)` runs the bundled `secantusd-mdb` binary as a
child process instead, with the same API.

What running in-process changes:

1. **One process's fate.** A WiredTiger panic or an abort in the server ends
   your Python process, where a child process would have died alone. That is
   what a database inside your process means (SQLite's model too).
   `embedded=False` is the answer when you need the isolation.
2. **Fork.** The server's threads do not survive `fork()`. A forked child can
   still connect to the parent's server through `server.uri`, and leaving the
   parent's `with` block there does nothing, but `server.stop()` in the child
   raises `ForkedServerError`. Use the `spawn` start method for
   `multiprocessing` (the default on macOS), or start the server after
   forking.
3. **Several servers** can run in one process on different `storage_path`s,
   each with its own WiredTiger connection. Two on the same path are refused
   by WiredTiger's lock, as two daemons are.
4. **Start-up is a library call**: no process to spawn, no banner to wait for.
   A server that cannot start (a port in use, a locked store) raises
   `ServerStartError`. `stop()` closes the store, WiredTiger's final
   checkpoint, and raises if that fails.

In-process, the server writes its messages to your process's stderr, and
`server.output` is empty.

The in-process server needs CPython 3.10 or later with the stable ABI. On a
Python without it, chiefly free-threaded CPython (3.13t, 3.14t), pip installs
a wheel without the extension: there `MongoServer()` raises
`ServerStartError` naming `embedded=False`, which works. It never falls back
to a child process on its own. The PostgreSQL server, `PostgresServer`, runs
as a child process.

### The `RustServer` handle (source builds)

`MongoServer` drives `RustServer`, the extension's lifecycle handle, which a
checkout built with the storage-engine flag
(`SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev`)
exposes as the top-level `_secantus_server` module. The project's own tests
and gauges use it directly. The accept loop runs on a GIL-released native
thread, and Python holds only a thin lifecycle handle:

```python
import _secantus_server
from pymongo import MongoClient

srv = _secantus_server.RustServer("./secantus-data", 0)  # storage_path, port
host, port = srv.address
client = MongoClient(host, port, directConnection=True)
client["mydb"]["users"].insert_one({"_id": 1, "name": "Joe"})
srv.stop()
```

Python is only the launcher — every byte of the request path (wire parse,
dispatch, operators, storage) is Rust. `pymongo` connects over real TCP
exactly as it would to the daemon.

## Constructor

```python
RustServer(
    storage_path,                  # WiredTiger home; created if absent
    port=0,                        # 0 = OS-assigned
    host="127.0.0.1",
    replica_set_name=None,         # None = plain standalone hello; pass
                                   # "secantus" to advertise the single-node
                                   # replica-set persona (change streams
                                   # need it)
    enable_oplog=True,             # oplog + change streams
    require_auth=False,            # SCRAM required on every command
    tls_cert_file=None,            # server TLS (pair with tls_key_file)
    tls_key_file=None,
    tls_ca_file=None,              # mTLS client-cert verification
    tls_require_client_cert=False,
    cache_size="4G",               # WiredTiger cache cap (filled lazily)
    session_max=1000,              # WiredTiger session cap
    sync_on_commit=False,          # fsync every commit (j:true semantics)
    oplog_nonlogged=None,          # oplog tables log=(enabled=false)
    data_nonlogged=None,           # WAL-log only the oplog; data tables
                                   # recover by replay from the last
                                   # stable checkpoint (mongod's split;
                                   # the default, see below)
    checkpoint_seconds=None,       # stable-checkpoint cadence (default 60)
    ttl_sweep_seconds=None,        # TTL monitor cadence; None = switched
                                   # off until setParameter turns it on
    noop_heartbeat_seconds=None,   # noop oplog heartbeat; None = none
    enable_test_commands=True,     # configureFailPoint & co.; mongod's
                                   # default (and MongoServer's) is False
)
```

The storage-mode kwargs default to `None` = defer to the matching
`SECANTUS_*` env var (`SECANTUS_OPLOG_NONLOGGED`,
`SECANTUS_CHECKPOINT_SECONDS`), so env-driven workflows are unchanged; an
explicit value wins over the environment for this server only. The
exception is `data_nonlogged`, which is on unless set to `False`, as
`secantusd-mdb` creates its stores: data tables that are not logged keep the
history a snapshot read at a past time (`readConcern: {level: "snapshot",
atClusterTime: ...}`) reads, which WiredTiger keeps for no logged table. A
store created with `data_nonlogged=False` refuses such a read (246).
Every write's oplog entry is written inside the write's own transaction, as
mongod writes it (the asynchronous oplog drainer was removed, 2026-11-20). A
store whose data tables are not logged (the default) refuses
`oplog_nonlogged=True`, because its data is
recovered by replaying the logged oplog; with the oplog out of the log too,
a crash would lose every write since the last checkpoint (RM-731). A
non-logged oplog needs `data_nonlogged=False`. The table-config modes (`oplog_nonlogged`,
`data_nonlogged`) are create-time-sticky: they shape fresh stores, and an
existing store keeps the mode it was created with (`data_nonlogged` is
recorded in the store and always wins on reopen).

A new store is initiated as mongod initiates a one-member replica set: the
oplog (held to its size in bytes, 192 MB on macOS and otherwise 5% of the
free disk; `replSetResizeOplog` changes it) begins with the `initiating set`
no-op, the `local.replset.*` and `local.system.rollback.id` collections are
written, and a checkpoint makes the oplog's catalog entry durable before any
write is accepted. Crash recovery replays the oplog from WiredTiger's
recovery timestamp, the stable timestamp of the last checkpoint, as
mongod's does.

Properties and methods: `srv.address` → `(host, port)` tuple, `srv.version`
→ the embedded crate version (also surfaced over the wire as
`buildInfo.secantusVersion`), `srv.stop()` → drain connections and close
storage, raising `RuntimeError` if WiredTiger's final checkpoint fails. The module attribute `_secantus_server.__version__` carries the
same version string.

## Tests under pytest-xdist

`port=0` plus a unique `storage_path`
per test (pytest's `tmp_path` gives both isolation and cleanup):

```python
import pytest

@pytest.fixture
def rust_server(tmp_path):
    import _secantus_server
    srv = _secantus_server.RustServer(str(tmp_path), 0)
    try:
        yield srv
    finally:
        srv.stop()
```
