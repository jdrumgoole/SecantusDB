# Embedded in a test

## From a Rust test

The `secantus-mdb` crate starts the server **in-process**: a real server on a
real TCP port, with a temporary store removed on drop, so any number of tests
can run in parallel. Add it as a dev-dependency and connect the official
`mongodb` driver:

```rust
let server = secantus_mdb::Server::start()?;   // temporary store, OS-assigned port
let client = mongodb::sync::Client::with_uri_str(server.uri())?;
client.database("mydb").collection("users")
    .insert_one(mongodb::bson::doc! {"_id": 1, "name": "Joe"})?;
```

`Server::builder()` sets a persistent store, a port, auth, TLS, the WiredTiger
cache and the background sweepers. Like the daemon, it expires TTL-indexed
documents every 60 seconds. A server started this way advertises a
single-node replica set named `secantus` and enables test commands, because
it is a test's server.

## From a Python test

The published `SecantusDB` wheel no longer contains the Rust server: it
carries only the Python servers. From a Python test, either run
`secantusd-rs` (from `cargo install secantus-mdb` or a release archive) as a
subprocess and point `pymongo` at it, or use the Python server's
`SecantusDBServer`, which has the same `pymongo` surface.

### The `RustServer` handle (source builds only)

A checkout built with the storage-engine flag
(`SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev`)
still has the embedded Python handle, which the project's own tests and
gauges use. The accept loop runs on a GIL-released native thread, and Python
holds only a thin lifecycle handle:

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
    oplog_async=None,              # background oplog drainer pool
    oplog_nonlogged=None,          # oplog tables log=(enabled=false)
    data_nonlogged=None,           # WAL-log only the oplog; data tables
                                   # recover by replay from the last
                                   # stable checkpoint (mongod's split)
    checkpoint_seconds=None,       # stable-checkpoint cadence (default 60)
)
```

The four storage-mode kwargs default to `None` = defer to the matching
`SECANTUS_*` env var (`SECANTUS_OPLOG_ASYNC`, `SECANTUS_OPLOG_NONLOGGED`,
`SECANTUS_DATA_NONLOGGED`, `SECANTUS_CHECKPOINT_SECONDS`), so env-driven
workflows are unchanged; an explicit value wins over the environment for
this server only. The table-config modes (`oplog_nonlogged`,
`data_nonlogged`) are create-time-sticky: they shape fresh stores, and an
existing store keeps the mode it was created with (`data_nonlogged` is
recorded in the store and always wins on reopen).

Properties and methods: `srv.address` → `(host, port)` tuple, `srv.version`
→ the embedded crate version (also surfaced over the wire as
`buildInfo.secantusVersion`), `srv.stop()` → drain connections and close
storage. The module attribute `_secantus_server.__version__` carries the
same version string.

## Tests under pytest-xdist

Same pattern as the Python server: `port=0` plus a unique `storage_path`
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
