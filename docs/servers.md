# The two servers

SecantusDB ships **two separate servers** that speak the same MongoDB wire
protocol. You run **one or the other** — there is no in-process engine
switching, and a client never sees a mix of the two.

- **The Rust server** — the flagship: a self-contained Rust server (its own
  wire / dispatch / cursors / accept loop over the pure-Rust engines and a
  WiredTiger-backed store) that runs its accept loop off the GIL. It ships in
  the `SecantusDB` wheel as an embedded lifecycle handle (`RustServer`) and
  as the standalone `secantusd-rs` binary; Python is only the launcher, never
  in the request path.
- **The Python server** — the original pure-Python `SecantusDBServer`. It is
  the readable reference: every operator, stage and error message lands here
  first.

Each is held to **`mongod`**, never to the other. Comparing the two servers
to each other detects drift; it never says which one is right.

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

| | Rust server | Python server |
| --- | --- | --- |
| Package | the `secantus-mdb` crate (crates.io), or a release archive | `pip install SecantusDB` |
| Run it as | `secantusd-rs` / `secantus_mdb::Server` | `SecantusDBServer` / `secantusd-py` |
| Conformance | **99.5%** of pymongo's own suite | see the [validation report](validation-report.md) |
| Speed | within 1.0×–3.2× of `mongod` per operation | 2×–27× |
| Request path | pure Rust (off the GIL) | pure Python |

Use the **Rust server** to run SecantusDB — in tests, in CI, in a container.
Reach for the **Python server** when you want to read how something works, or
need one of the few features only it has (listed below and in the
[Feature comparison](feature-comparison.md)). Speed figures are from
[Benchmark](benchmark.md).

The same split exists on the PostgreSQL side: the Rust PostgreSQL server
(`secantusd-pg`) and the Python one
(`SecantusPGServer` / `secantusd-py-pg`). See the
[SQL / PostgreSQL interface](sql.md).

## Versioning

The two servers are **separate deliverables on independent version lines**;
they diverged at `0.5.2` and advance independently:

- **Python server** — `0.6.0bN` (PEP 440). This is the **PyPI package** version
  in `pyproject.toml` / `secantus.__version__`.
- **Rust server** — `0.5.3-beta.N` (SemVer pre-release), carried in lockstep
  across the `crates/*` workspace and surfaced over the wire as
  `buildInfo.secantusVersion`, by the `secantusd-rs --version` flag, and by
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

Or as a daemon — `pip install` puts a `secantusd-py` script on `PATH`:

```bash
secantusd-py --host 127.0.0.1 --port 27017
```

See [Quickstart](quickstart.md) and [Installation](installation.md).

### Rust server

The Rust server ships as the `secantus-mdb` crate on crates.io, not in the
Python wheel. `cargo install` builds it (WiredTiger included) and puts the
`secantusd-rs` daemon on `PATH`:

```bash
cargo install secantus-mdb --version 0.5.3-beta.177
secantusd-rs --host 127.0.0.1 --port 27017
```

From a Rust test, the same crate starts the server in-process as
`secantus_mdb::Server`. From a Python test, run `secantusd-rs` as a
subprocess and point `pymongo` at it, or use the Python server, which has
the same `pymongo` surface.

Standalone `secantusd-rs` archives (no Python needed) are attached to the
`secantusdb-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases). Both
Mongo daemons read the same `secantusd.toml` config (see
[Configuration](configuration.md)).

### SQL / PostgreSQL servers

The Rust PostgreSQL server is the `secantus-pg` crate on crates.io (`cargo
install secantus-pg --version 0.1.0-beta.8`), and also ships as a standalone
`secantusd-pg` archive on
the `secantusd-pg-v*` tags on
[GitHub Releases](https://github.com/jdrumgoole/SecantusDB/releases):

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432
```

To offer TLS, give it a PEM certificate chain and its private key (no
passphrase):

```bash
secantusd-pg ./secantus-pg-data 127.0.0.1:5432 \
    --tls-cert-file server.crt --tls-key-file server.key
```

A client that asks for TLS then gets it (`sslmode=require`, `verify-ca`,
`verify-full`); one that does not ask is still served in the clear. Without
the pair the server answers a TLS request with "not supported", as PostgreSQL
does with `ssl = off`. Client certificates and SCRAM channel binding are not
supported. The TLS options are on `main`; the released binaries up to beta 8
do not have them.

The Python one needs the `sql` extra (`pip install "SecantusDB[sql]"`) and
runs as `secantusd-py-pg`:

```bash
secantusd-py-pg --host 127.0.0.1 --port 5432 --storage-path ./secantus-data
```

See the [SQL / PostgreSQL interface](sql.md).

## What each server does **not** support

Both servers share the project-wide non-goals — anything that depends on **real
cluster topology** (multi-node replica sets, sharding, elections, cross-node
oplog), auth mechanisms beyond SCRAM (SHA-1 / SHA-256) and `MONGODB-X509`,
`OP_COMPRESSED`, text / hashed / wildcard indexes, and
`$where` / `$function` / `$accumulator` / JS `mapReduce` (no embedded JS
runtime).
These are out of scope for **both** servers; the per-feature detail lives in
[Compatibility](compatibility.md).

### Python server

The Python server implements the full in-scope wire surface. Its remaining
divergences are the stopgaps and known edge cases enumerated in
[Compatibility](compatibility.md) — the `_id` numeric-type bridge is
undefined for `NaN` / infinity, `top` counters are always zero, and a handful
of date-format and
`$group`-ordering edge cases. There is no *feature* the Python server is
missing relative to the in-scope set; it is the conformance reference the Rust
server is measured against.

### Rust server

The Rust server passes 99.5% of pymongo's suite. The remaining *feature* differences (full three-way matrix in the
[Feature comparison](feature-comparison.md)) are:

- **Point-in-time restore over the wire** — `secantusAdmin.restoreToTimestamp`
  is Python-server-only; the Rust server does the same restore via the
  `secantusd-rs restore` CLI subcommand.
- **Session lifecycle** — `endSessions` / `refreshSessions` / `killSessions`
  are acknowledged no-ops on the Rust server; the Python server tracks
  sessions with a 30-minute idle TTL.
- **Operator edges** — a handful of `$dateFromString` / `$dateToString`
  format directives, Decimal128 arithmetic edges, and mixed-type sort
  orderings the Rust engine rejects rather than risk diverging from
  `mongod`.
- **Thinner diagnostics** — `serverStatus` / `dbStats` / `collStats` return a
  smaller subset of fields than the Python server's replies.

Both servers mint resume tokens in SecantusDB's own `{s, t, n, k}` layout
rather than mongod's keystring format — tokens round-trip within SecantusDB but
cannot be presented to a real `mongod` (or vice versa).

The current Rust-server pass rate, and the exact set of failing pymongo tests,
is regenerated each run into the
[Rust-server validation report](validation-report-rust-server.md). The gap
against the [Python-server report](validation-report.md) is the canonical,
machine-checked statement of what the Rust server doesn't support yet.
