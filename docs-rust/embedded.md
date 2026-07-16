# Embedded in Python

The Rust server can run **inside a Python process** — the accept loop runs
on a GIL-released native thread, and Python holds only a thin lifecycle
handle. Your test spawns a real Rust server on a real TCP port in a couple
of lines, with no subprocess to manage.

The handle ships in the storage-engine build of the wheel
(`SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON`, see
[Installation](installation.md)):

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
    replica_set_name=None,         # None = single-node RS persona "secantus";
                                   # pass a name to override, or use the
                                   # standalone persona via the daemon flag
    enable_oplog=True,             # oplog + change streams
    require_auth=False,            # SCRAM required on every command
    tls_cert_file=None,            # server TLS (pair with tls_key_file)
    tls_key_file=None,
    tls_ca_file=None,              # mTLS client-cert verification
    tls_require_client_cert=False,
)
```

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
