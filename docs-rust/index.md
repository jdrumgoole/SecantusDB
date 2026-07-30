# SecantusDB Rust server

The SecantusDB **Rust server** is a whole second implementation of the
SecantusDB database — wire parsing, command dispatch, cursors, change
streams, the operator engines, and WiredTiger storage, all in Rust with no
Python in the request path. It speaks the same MongoDB wire protocol as
[the Python server](https://secantusdb.com/docs/), passes the same
unmodified driver-conformance suites (99.5% of pymongo's own tests — level
with the Python server), and ships as a single static-WiredTiger binary:
`secantusd-rs`.

```bash
# From a prebuilt archive (Linux x86_64 / macOS arm64):
./secantusd-rs --port 27017 --storage-path ./secantus-data
```

```python
# Or bundled in the Python wheel:
#   pip install SecantusDB   (storage-engine build)
import _secantus_server
from pymongo import MongoClient

srv = _secantus_server.RustServer("./secantus-data", 0)  # port 0 = OS-assigned
host, port = srv.address
client = MongoClient(host, port, directConnection=True)
client["mydb"]["users"].insert_one({"_id": 1, "name": "Joe"})
srv.stop()
```

Every MongoDB driver that talks to the Python server talks to the Rust
server unchanged — same `OP_MSG` handshake, same commands, same error
codes, same on-disk WiredTiger semantics.

And it is fast: on the six-workload benchmark the Rust server runs at
**~0.9×–2.1× of real `mongod`** per operation (delete beats standalone
`mongod` at 0.9×, `$group` is at parity, update near parity — after a
mimalloc allocator, LTO, and profile-guided optimization cut the
BSON-materialization allocation and hot-path branch cost), sustains
**~2.6× multi-writer scaling fully durable** (monotonic to eight
writers), and is roughly 3.5×–17× faster than the Python server
workload-for-workload — measured end-to-end through `pymongo` on
on-disk WiredTiger. Numbers and methodology:
[Benchmark](https://secantusdb.com/docs/benchmark.html) and
[Concurrency](https://secantusdb.com/docs/concurrency.html).

## Where this fits

SecantusDB ships **two separate servers** on independent version lines. The
Python server is the conformance reference and the default choice; the Rust
server is the same database built for speed and dependency-free standalone
deployment. The full decision guide is
[The two servers](https://secantusdb.com/docs/servers.html), and the
[feature comparison](https://secantusdb.com/docs/feature-comparison.html)
maps both servers against real MongoDB, feature by feature.

```{toctree}
:maxdepth: 2
:caption: Contents

installation
running
embedded
security
recovery
conformance
architecture
releases
```
