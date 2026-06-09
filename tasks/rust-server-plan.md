# Rust server plan — two separate servers, not one selectable engine

**Status: active. This document supersedes the in-process selectable-engine
model** described in `tasks/rust-rewrite-plan.md` §2 / §10, the "dual-engine at
`Storage` granularity" decision in `tasks/rust-rewrite-phase3-scoping.md` §3 and
`tasks/rust-rewrite-phase4-scoping.md`, and the §5e "Python `Storage` adapter +
`secantus.engine` selection" cutover. Where those docs say "select the engine
process-wide via `secantus.engine` / `SECANTUS_ENGINE`" or "two `Storage`
implementations behind one interface," read **this** instead.

The *engine-porting* history in those docs (the pure-Rust crates, the WT-FFI
decisions, the storage sub-phases 2a–5e, the geo slices) is still valid and
still the foundation — only the **integration model** changes.

---

## 1. The decision

There are **two completely separate servers**, and a user runs **one or the
other** — never a mix, never a per-operator/per-`Storage` selection inside one
request path:

- **The Python server** — the *original* `SecantusDBServer`: `server.py` +
  `wire.py` + `commands.py` + the pure-Python `Storage` and the pure-Python
  operator engines (`query` / `update` / `expressions` / `projection` / `diff` /
  `sortkey` / `aggregate`). **No Rust in the request path.** This is the
  reference implementation and stays first-class and permanent.

- **The Rust server** — a *whole, self-contained* server written in Rust: its
  own wire layer, command dispatch, cursor registry, accept loop, and the
  already-ported pure-Rust `secantus-core` + `secantus-storage` + `secantus-wt`
  crates underneath. **No Python in the request path, no PyO3 in the hot path,
  no fallback into Python operators.**

### Why this replaces the old model

The old plan made the two implementations co-resident in one process, selected
per-component (`secantus.engine`) with a graceful per-call `Fallback` to Python.
For *pure operator engines* that was safe and it shipped (Phases 1–2). But
extending it to **storage and the server** forced the "5e adapter": a Python
`Storage` shell routing every data op across the PyO3 seam into `RustStorage`,
with `EngineFallback` bouncing unsupported ops *back* into Python operators
running over Rust-scanned docs. That is a half-Python/half-Rust request path —
the worst of both: the marshalling tax the byte-seam was meant to kill, two code
paths entangled in one process, and no clean "this is the Rust server" artifact.

Two separate servers removes the entanglement. Each server is internally
coherent and independently testable, and the Rust server is exactly the
standalone artifact the crate split was always aiming at (`secantus-core` /
`secantus-storage` are deliberately PyO3-free precisely so a standalone server
can reuse them).

## 2. The Python interface is a thin layer over the Rust server

The CLAUDE.md ergonomic — *"starting a server in a test should be one or two
lines, with no external processes to manage"* — is preserved for the Rust
server by a **thin embedded lifecycle handle**, not by putting Python in the
request path:

- A small PyO3 crate (`crates/secantus-server-py`) exposes a `#[pyclass]` with
  only **`start(storage_path, port=0, …) -> address`**, **`stop()`**, `.address`
  / `.uri`, and the context-manager protocol. That is the *entire* Python
  surface — lifecycle, not operators.
- On `start()`, the Rust server's TCP accept loop runs on a **Rust thread inside
  the Python process** (GIL released via `Python::allow_threads`), exactly as
  today's pure-Python `SecantusDBServer` runs its accept loop on a daemon
  thread. The user's `pymongo` client connects over **real TCP** to that
  in-process listener. `port=0` (ephemeral) and a `tmp_path` storage dir work
  unchanged; shutdown is deterministic.

This keeps the byte path closed (`socket bytes → Rust wire → Rust dispatch →
Rust storage → Rust BSON reply → socket bytes`) — Python is only ever the
*launcher*. It is **not** the rejected selectable engine: there is no Python
operator code in the path and no `EngineFallback`.

Two ways a user reaches the Rust server, both driving the *same* Rust core:

1. **From Python** — `from secantus import …` a thin wrapper that boots the
   embedded handle (the test/dev ergonomic, what pytest uses).
2. **Standalone** — a `secantusdb` Rust binary (`main` over the same crates) for
   non-Python users. Free once the server is a library crate.

*(Subprocess model — Python spawns the standalone binary and hands back a URI —
was considered and rejected: it reintroduces an external process to manage,
against the design constraint. Embedded handle is the chosen path.)*

## 3. What is kept, what is dropped

**Kept (the foundation — unchanged):**
- The pure-Rust crates: `secantus-core` (engines + geo + aggregation), `secantus-
  storage` (the full WT-backed `Storage`: CRUD, indexes, geo, oplog/change-stream
  storage, users/roles/profiling, lifecycle, stats), `secantus-wt` (the WT FFI).
  All of the 2a–5e porting work lands directly under the Rust server.
- **The leaf-engine parity suites** (`tests/test_rust_*_parity.py`) — Rust
  engines stay pinned **byte-for-byte** to the pure-Python engines as the
  fine-grained correctness oracle (the maintainer chose to keep these). They
  import `_secantus_core` to run the Rust engine and diff it against the Python
  one; that is their *only* remaining reason to exist, and it is sufficient.
- The WT-FFI engine choice (option A), faithful on-disk encodings (`sortkey` /
  geo golden-pinned), and the 1:1 concurrency port.
- The decision to **bundle** the compiled Rust artifact into the `secantus`
  wheel behind an off-by-default CMake flag, reusing the wheel's existing
  vendored-WiredTiger build (see `rust-rewrite-phase4-scoping.md` "wheel-matrix
  gate"). That mechanism now ships the Rust *server* extension, not a storage
  adapter.

**Dropped / retired (the selectable-engine machinery):**
- **`secantus.engine` process-wide selection in the Python request path** — the
  per-component shims (`query.py` / `update.py` / … delegating to `_secantus_core`
  when `enabled()`), `SECANTUS_ENGINE` / `SECANTUS_RUST_<COMPONENT>` routing, and
  `SecantusDBServer(engine=…)`. The Python server reverts to **pure Python**. The
  `_secantus_core` PyO3 bindings are **retained only as the parity-test vehicle**,
  not wired into any server's request path.
- **The "5e adapter"** — the Python `Storage` over `RustStorage`, `EngineFallback`
  routing, and the *fat* `secantus-storage-py` surface (the whole `Storage`
  interface exposed to Python). The Rust server calls `secantus-storage`
  **natively in Rust**; the only Python-facing surface is the thin lifecycle
  handle (§2). `secantus-storage-py` collapses accordingly (kept, if useful, only
  for storage-crate unit smokes — not as a production path).

> **Note / confirm:** retiring the in-process per-operator accelerator means the
> Python server no longer gets the Rust speedup — that path is now the Rust
> server's job. This follows from "two completely separate servers, original
> server based on Python." Flagged here in case the intent was to *also* keep the
> per-operator accelerator on the Python server; the plan assumes not.

## 4. Rust-server build-out (the new phase sequence)

The engines + storage are done (pure-Rust crates, parity-pinned). What remains is
the **server above the storage**, all net-new Rust. Each slice gates on running
the relevant pymongo-driven suite **against the Rust server** (via the embedded
handle, `port=0`, `tmp_path`) in CI / on a WT-capable machine.

- **R1 — Wire layer** (`crates/secantus-wire`). Port `wire.py`: 16-byte
  little-endian header, `OP_MSG` (2013) kind-0 + kind-1 document-sequence
  parse/build, legacy `OP_QUERY` (2004) parse + `OP_REPLY` (1) build for the
  pymongo handshake, bounds checks. Pure bytes-in/bytes-out → unit-testable
  without a socket. Pin against `wire.py` with shared byte fixtures.

- **R2 — Command dispatch** (`crates/secantus-commands`, or a module of the
  server crate). Port `commands.py`: the dispatch table keyed on the first doc
  key, the handshake family (`hello`/`isMaster`/`ping`/`buildInfo`/…), CRUD
  (`insert`/`find`/`update`/`delete`/`count`/`drop`/`aggregate`/`findAndModify`/
  `listCollections`/…), the error contract (handler error →
  `{ok:0, errmsg, code, codeName}`; unknown command → `59 CommandNotFound` so the
  connection survives — a Rust `CommandError` + top-level `catch` in dispatch).
  The widest slice; sub-slice by command family and keep `test_crud.py` /
  `test_aggregate.py` / `test_commands*.py` green against the Rust server per
  family.

- **R3 — Cursor registry + change-stream tailable plumbing** (in the server
  crate). Port `cursors.CursorRegistry` (int64 id → remaining batch, idle-TTL
  pruning, injectable clock) to a Rust struct owned by the server, and the
  tailable change-stream producer (oplog tail → `changestreams::project`, already
  in `secantus-storage`) driving `getMore`/`killCursors`. The blocking
  `awaitData` wait uses the storage `wait_for_oplog` / `notify_oplog_waiters`
  condvar primitive (landed in 5e-gap-c). Gate: `test_change_streams.py`.

- **R4 — Accept loop + connection threads + TLS** (`crates/secantus-server`). Port
  `server.py`: TCP accept on a daemon thread, thread-per-connection (1:1 with the
  Python model), per-request `CommandContext`, graceful shutdown. TLS via
  `rustls` (+ `rustls-pemfile`); mTLS peer-cert subject-DN extraction reproduced
  for X509 auth. Gate: `test_server.py` / `test_tls*.py`.

- **R5 — Auth** (in the server crate or `crates/secantus-auth`). SCRAM-SHA-1/256
  (`hmac` / `sha2` / `pbkdf2`), MONGODB-X509, and the RBAC checks. Port `auth.py`
  / `rbac.py`. Pin against `test_auth.py` / `test_x509_auth.py` (tight suites).
  Uses the already-ported users/roles storage (5e-gap-a).

- **R6 — Thin Python embed handle** (`crates/secantus-server-py`). The `#[pyclass]`
  of §2: `start`/`stop`/`address`/`uri` + context manager, accept loop on a
  GIL-released Rust thread. A Python wrapper in `secantus` exposes it with the
  same constructor ergonomics so pytest can boot the Rust server in one or two
  lines. This is the seam the conformance gate runs through.

- **R7 — Standalone `secantusdb` binary.** A `main` over the same crates for
  non-Python users. Mostly free once R1–R5 are library crates; adds CLI arg
  parsing (port the relevant bits of `cli.py`).

- **R8 — Conformance gate (go/no-go).** Run the **unchanged** pymongo-driven
  suites (`test_crud.py` / `test_storage.py` / `test_indexes.py` /
  `test_geo_index.py` / `test_change_streams.py` / `test_aggregate.py` / …) and
  the **pymongo + Go/Node/Java/Ruby driver gauges** against the Rust server. The
  headline "MongoDB compatibility" number must not regress vs the Python server.
  This is the definition of "the Rust server is correct."

**Leftover storage work folded in** (was Phase-4 tail): re-home `$lookup` /
`$geoNear` / `$out` / `$merge` storage-backed aggregation into `secantus-storage`
(geo-4 + the lookup/merge acceleration), and the remaining 5e gaps
(`checkpoint` / `close` / `create_archive`) as the Rust server needs them.

## 5. Testing & CI strategy

- **Two green servers.** The pymongo + 6-driver gauges run **twice** — once
  against the Python server (the existing CI matrix, unchanged) and once against
  the Rust server (the R8 gate). Neither may regress.
- **Parity suites stay** as the operator-level oracle (§3): Rust engines pinned
  byte-for-byte to Python, independent of either server. They catch divergences
  finer-grained than the wire gauges can.
- **Golden vectors** for the on-disk/byte contracts (`sortkey`, geo cells, resume
  tokens) remain checked-in and asserted in `cargo test`.
- CI lanes: the existing `rust` (core parity) + `rust-storage` (WT-backed storage
  crate) jobs stay; add a **`rust-server`** job that builds the server extension
  (reusing the wheel's vendored WT) and runs the pymongo gauge against it. The
  `storage-engine` bundling job generalises to bundle the *server* extension.

## 6. Open items to confirm

1. **Fate of the in-process per-operator accelerator** — the plan retires it from
   the Python server (Python server = pure Python; all Rust = the Rust server),
   keeping `_secantus_core` only as the parity-test vehicle. Confirm this is the
   intent and not "keep the per-operator speedup on the Python server too." (§3
   note.)
2. **Naming** — how the two servers are surfaced in the public Python API (e.g.
   `SecantusDBServer` stays pure-Python and a new `RustSecantusDBServer` / a
   `secantusdb --rust` flag boots the Rust one), so "pick one" is explicit and
   discoverable. To be settled at R6.
3. **Whether the Python server stays at feature parity going forward**, or becomes
   a frozen reference once the Rust server passes the gate. (The product can keep
   both permanently; this is about where *new* feature work lands.)
