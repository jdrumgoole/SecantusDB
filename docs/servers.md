# The two servers

SecantusDB ships **two separate servers** that speak the same MongoDB wire
protocol. You run **one or the other** — there is no in-process engine
switching, and a client never sees a mix of the two.

- **The Python server** — the original pure-Python `SecantusDBServer`
  (this PyPI package). It is the conformance leader and the default choice.
- **The Rust server** — a self-contained Rust server (its own wire / dispatch
  / cursors / accept loop over the pure-Rust engines and a WiredTiger-backed
  store) that runs its accept loop off the GIL. Its Python ergonomic is a thin
  embedded lifecycle handle (`start` / `stop` / `address`); Python is only the
  launcher, never in the request path.

Both store data on the same vendored **WiredTiger** engine `mongod` ships, so
the on-disk durability story is identical. The difference is the layers above
storage — command dispatch, query planning, the operator engines — which are
Python in one server and Rust in the other.

:::{note}
The old in-process accelerator (`SECANTUS_ENGINE=rust` /
`SecantusDBServer(engine=...)`) has been **retired** in favour of this
two-server split. The Python server is always pure-Python; the Rust engines
live only in the Rust server.
:::

## Which one should I use?

| | Python server | Rust server |
| --- | --- | --- |
| Package | `pip install SecantusDB` (always present) | built behind a flag / `pip install "secantus[rust]"` |
| Maturity | conformance leader — **99.2%** of pymongo's own suite | **92.0%** of the same suite (catching up) |
| Best for | the default: tests, dev, embedded apps, single-node prototypes | throughput-sensitive workloads where the Rust hot path matters |
| Request path | pure Python | pure Rust (off the GIL) |

Use the **Python server** unless you have a specific reason not to. It passes a
higher fraction of the driver suites and supports the full in-scope feature set
described in [Compatibility](compatibility.md). The Rust server is faster
per operation (see [Benchmark](benchmark.md)) but is still closing the
conformance gap below — pick it only when its remaining gaps don't touch your
workload.

## Versioning

The two servers are **separate deliverables on independent version lines**;
they diverged at `0.5.2` and advance independently:

- **Python server** — `0.5.3bN` (PEP 440). This is the **PyPI package** version
  in `pyproject.toml` / `secantus.__version__`.
- **Rust server** — `0.5.3-beta.N` (SemVer pre-release), carried in lockstep
  across the `crates/*` workspace and surfaced over the wire as
  `buildInfo.secantusVersion`, by the `secantusdb-rs --version` flag, and by
  the embedded handle's `RustServer.version`.

A change that touches only one server bumps only that server's version.

## Running each server

### Python server

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=27017) as server:
    client = MongoClient(server.uri)
    client["mydb"]["users"].insert_one({"_id": 1, "name": "Joe"})
```

Or as a daemon — `pip install` puts a `secantusdb` script on `PATH`:

```bash
secantusdb --host 127.0.0.1 --port 27017
```

See [Quickstart](quickstart.md) and [Installation](installation.md).

### Rust server

The Rust server is **not** in the default wheel. Build it with the storage-engine
flag on:

```bash
SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev
```

A flag-on build exposes the embedded handle and a `secantusdb-rs` daemon on
`PATH` (distinct from the pure-Python `secantusdb` console script):

```python
import _secantus_server
from pymongo import MongoClient

srv = _secantus_server.RustServer("./secantus-data", 0)  # storage_path, port (0 = OS-assigned)
host, port = srv.address
client = MongoClient(host, port, directConnection=True)
# ... use it ...
srv.stop()
```

```bash
secantusdb-rs --host 127.0.0.1 --port 27017
```

## What each server does **not** support

Both servers share the project-wide non-goals — anything that depends on **real
cluster topology** (multi-node replica sets, sharding, elections, cross-node
oplog), auth mechanisms beyond `SCRAM-SHA-256` / `MONGODB-X509`, authorization
(RBAC), `OP_COMPRESSED`, text / hashed / wildcard indexes, and
`$where` / `$function` / `$accumulator` / `mapReduce` (no embedded JS runtime).
These are out of scope for **both** servers; the per-feature detail lives in
[Compatibility](compatibility.md).

### Python server

The Python server implements the full in-scope wire surface. Its remaining
divergences are the stopgaps and known edge cases enumerated in
[Compatibility](compatibility.md) — `$lookup` is a hash-join rather than an
index-driven join, the `_id` numeric-type bridge is undefined for `NaN` /
infinity, `top` counters are always zero, and a handful of date-format and
`$group`-ordering edge cases. There is no *feature* the Python server is
missing relative to the in-scope set; it is the conformance reference the Rust
server is measured against.

### Rust server

The Rust server passes 92.0% of pymongo's suite versus the Python server's
99.2%. **That gap is the Rust server's remaining to-do list** — features the
Python server already supports but the Rust server does not yet:

- **DDL change-stream events (`showExpandedEvents`)** — the Rust server's
  `create_index` / `drop_index` / `collMod` write no oplog `c` entry, so a
  `showExpandedEvents: true` change stream never sees `createIndexes` /
  `dropIndexes` / `modify` events. (`create` / `drop` collection events *are*
  emitted.)
- **Large-event splitting** — `splitLargeChangeStreamEvents` always reports a
  single fragment (`of: 1`); the Rust server can't yet split a change event
  larger than the 16 MB BSON limit.
- **Read concern / write concern** — accepted on the wire for compatibility,
  then ignored (no snapshot / majority semantics applied).
- **Timeseries `_id` non-uniqueness** — the Rust storage layer keys every
  document by `_id`, so it rejects the duplicate `_id`s a timeseries
  collection relies on (the Python server suffixes the key to allow them).
- Assorted CRUD / cursor / introspection edge cases that make up the rest of
  the conformance gap (`test_cursor`, `test_collection_management`,
  `test_database` — see the
  [Rust-server validation report](validation-report-rust-server.md) for the
  current failing-test list).

Both servers mint resume tokens in SecantusDB's own `{s, t, n, k}` layout
rather than mongod's keystring format — tokens round-trip within SecantusDB but
cannot be presented to a real `mongod` (or vice versa).

The current Rust-server pass rate, and the exact set of failing pymongo tests,
is regenerated each run into the
[Rust-server validation report](validation-report-rust-server.md). The gap
against the [Python-server report](validation-report.md) is the canonical,
machine-checked statement of what the Rust server doesn't support yet.
