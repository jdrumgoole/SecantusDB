# Plan: Rewriting the SecantusDB server core in Rust

Status: **proposal / design** — no code changes yet. This document is the
strategy. It does not commit us to a timeline; it commits us to an order, a
set of seams, and a list of decisions that have to be made before the first
line of Rust lands.

---

## 1. What we are actually rewriting, and what we are not

A quick census of the tree (source only, excluding tests):

| Layer | Files | ~LOC | Native deps | Rewrite priority |
|---|---|---|---|---|
| Pure operator engines | `query.py`, `update.py`, `expressions.py`, `projection.py`, `diff.py`, `sortkey.py`, `paths.py`, `collation.py` | ~3,600 | `bson` | **First** (leaf, no I/O) |
| Aggregation | `aggregate.py` | ~2,100 | `bson` | After engines |
| Geo | `geo.py`, `geo_index.py` | ~810 | `shapely`, `s2sphere`, `bson` | With/after engines |
| Storage | `storage.py`, `cursors.py` | ~5,100 | **WiredTiger (C/SWIG)**, `bson` | High-risk; mid |
| Change streams / oplog | `changestreams.py` | ~530 | `bson` | With storage |
| Command layer | `commands.py`, `auth.py`, `rbac.py`, `sessions.py`, `failpoints.py`, `metrics.py`, `connreg.py`, `logbuf.py`, `config.py` | ~6,900 | `bson` | After engines+storage |
| Wire + server | `wire.py`, `server.py` | ~760 | `bson`, sockets, `ssl` | Last (becomes thin) |
| CLI | `cli.py`, `restore_cli.py`, `__main__.py` | ~370 | — | Thin shim |
| **Admin web app** | `admin/**` | ~5,000 | FastAPI, uvicorn, pywebview, httpx | **Out of scope** (see §9) |

**Total in-scope core: ~20,000 LOC of Python.** The admin GUI (~5k LOC) is an
optional `admin` extra — a local FastAPI app — and is explicitly *not* part of
this rewrite. It will keep talking to the core through the core's Python API.

The conformance oracle is unchanged and is our single biggest asset: the
`tests/` suite (~25k LOC, almost all **pymongo-driven integration tests**) plus
six driver-conformance gauges (pymongo embedded + Go/Node/Java/Ruby/Rust
subprocess). These are **language-agnostic** — they drive the server over the
wire and assert on responses. A Rust core that passes them is, by the project's
own definition of "correct," done. We do not rewrite the tests; they are the net.

---

## 2. The pivotal decision: embeddable extension, not standalone binary

This is the single most consequential choice and it shapes everything below.

`CLAUDE.md` states the product's reason to exist in three hard constraints:

> *"Ease of use for the beginning programmer: starting a server in a test
> should be one or two lines, with no external processes to manage."*
> *"fast, ephemeral, **in-process** MongoDB behaviour for tests."*

A pure standalone Rust `mongod`-surrogate **binary** would violate this: a
pytest using `SecantusDBServer(...)` as an in-process object on `port=0` with a
`tmp_path` storage dir is the core ergonomic. If the server became an external
process you'd have to spawn/reap it, manage ports, and lose `tmp_path`
isolation — exactly what the product promises to avoid.

**Recommendation: build the Rust core as a Python extension module via
[PyO3](https://pyo3.rs) + [maturin](https://www.maturin.rs), and keep
`SecantusDBServer` as a thin Python (or `#[pyclass]`) wrapper.** The Rust core
owns the accept loop, storage, and request handling; Python keeps the
two-line ergonomic. We optionally *also* ship a standalone `secantusdb` binary
(same Rust core, different `main`) as a bonus for non-Python users — that's
free once the core is a library, but it is not the primary artifact.

Why this also makes the *migration* tractable:

- PyO3 lets us replace **one subsystem at a time** behind a stable Python API,
  with the pymongo suite green at every commit (strangler-fig). We never have a
  "big bang" cutover.
- It frees the GIL for true multi-core request handling — a real performance
  win the current thread-per-connection-under-GIL design can't get.

The decision and its alternatives are revisited in §10. Everything from §3 on
assumes the PyO3 path.

---

## 3. The marshalling trap, and the seam that avoids it

The naive way to call Rust from Python is value-by-value: convert each Python
`dict`/`ObjectId`/`Decimal128` into a Rust `bson::Bson` and back on every call.
During a long hybrid period that is **death by a thousand conversions** and it
also re-introduces exactly the type-fidelity risk the project's "opaque BSON
blob" design was built to eliminate.

The escape hatch is already baked into the architecture: **documents are opaque
BSON bytes end-to-end.** The wire layer speaks bytes; storage stores
`bson.encode(doc)` bytes; the only place we ever materialise a Python `dict` is
inside the pure engines. So the seam between Python and Rust must be **fat and
byte-oriented**, never per-field:

- Pure engines exposed to Python as `fn(doc_bytes, spec_bytes) -> bytes/bool`,
  operating on `bson::RawDocument` (zero-copy borrow over the buffer) internally.
- Storage exposed as bytes-in / bytes-out.
- During the hybrid period, Python decodes to a `dict` **only** when a
  still-Python layer genuinely needs the dict; Rust↔Rust calls stay in bytes.

End state: a request flows `socket bytes → Rust wire parse → Rust dispatch →
Rust storage (WT) → Rust BSON reply → socket bytes`, never entering Python
except to construct/drive the server. The Python `dict` representation that
pervades today's code is revealed as an implementation detail and disappears.

This reframes the migration order. Two equally valid directions:

1. **Inside-out** (leaf engines first): lowest risk, proves PyO3 + `bson`-crate
   fidelity on pure code, but the fat byte seam means each migrated engine pays
   a decode/encode tax until its callers are also Rust.
2. **Outside-in** (wire/accept loop first, dispatch to Python via callback):
   gets the byte seam "for free" at the socket immediately, but front-loads the
   server lifecycle and threading rewrite.

**Recommendation: inside-out for engines (Phases 1–3), then a single
outside-in flip (Phase 5) once storage is Rust**, so the byte path closes up
without a long window of double-marshalling on the hot path. See the roadmap.

---

## 4. The two genuine hard problems

Everything else is "rewrite a pure function and diff it against the Python one."
These two are not.

### 4.1 WiredTiger from Rust

WT is the spine of `storage.py` and there is **no mature `wiredtiger` Rust
crate.** We already vendor the C source as a submodule (`vendor/wiredtiger`,
mongodb-7.0.33) and build it via CMake/scikit-build into a SWIG Python module.
Options, in recommended order:

- **(A) FFI to the vendored C library via `bindgen` + a `build.rs`.**
  Reuse the *exact* vendored WT we already ship; `build.rs` drives the existing
  CMake (or links the static lib it produces) and `bindgen` generates the FFI.
  We then write a small safe-Rust wrapper around the handful of WT calls
  `storage.py` actually uses (open, session, cursor, `insert`/`search`/`remove`/
  range scan, `reset`, checkpoint). **This preserves the "same engine MongoDB
  uses" design principle and keeps on-disk format identical.** Highest fidelity,
  most build plumbing. *Recommended.*
- **(B) Swap to a Rust-native ordered KV store** (`redb`, `rocksdb` via
  `rust-rocksdb`, or LMDB via `heed`). The storage design only needs an
  *ordered* byte-key/byte-value store — the byte-sortable `sortkey` encoding is
  engine-agnostic. This deletes ~all the WT build plumbing and the SWIG/CMake
  complexity, and would massively simplify wheels. **But** it abandons the
  stated design value ("on-disk semantics line up with real `mongod`") and is a
  visible product change. Viable *only* with explicit sign-off (see §10).
- **(C) Keep storage in Python, call it from Rust via PyO3 during migration.**
  A transitional crutch, not an end state — re-entering Python on the hot path
  defeats the point. Useful only to unblock the wire/dispatch rewrite before
  storage is ported.

Whichever we pick, the **on-disk format is a compatibility decision** (§4.2).

### 4.2 BSON / sortkey byte-fidelity

Two sub-risks:

- **`bson` crate vs pymongo `bson`.** The Rust `bson` crate (MongoDB-maintained)
  must round-trip ObjectId, Decimal128, int32-vs-int64, Date-with-tz, Binary
  subtypes, Regex+options, Timestamp, MinKey/MaxKey, and **key ordering** /
  **duplicate keys** exactly as pymongo emits and expects. This is the entire
  "pymongo can't tell us apart" thesis. *De-risk with a spike (Phase 0):* a
  differential round-trip harness feeding the same BSON bytes through pymongo
  and the Rust crate and asserting byte-equality, run over corpora harvested
  from the existing test fixtures.

- **`sortkey` is an on-disk format.** `encode_value` / `encode_compound`
  produce the bytes that key the WT index-entries table. The "lexical decimal"
  numeric encoding (int/long/double/Decimal128 collide on equal value) and the
  null-escaping (`\x00 → \x00\xff`) are **byte-exact contracts**. If we keep WT
  tables (option A) **and** want to read existing databases, the Rust encoder
  must be byte-identical — pin it with **golden test vectors** generated from
  the Python encoder (a few thousand values across every type and boundary:
  NaN/±Inf, Decimal128 edge cases, mixed-direction). If we accept a one-time
  **storage-format-version bump** (reasonable — the product is for *ephemeral
  test* data; nobody is carrying a SecantusDB database across the rewrite), the
  encoder still must be *self-consistent and order-correct* but need not match
  the old bytes. **Recommendation: bump the format version, drop
  backward-compat, but reproduce the encoding faithfully anyway** so the cross-
  type sort tests (`test_indexes.py`, `test_sort_with_collation.py`) pass
  unchanged. Document the break in `tasks/backlog.md`.

---

## 5. The other things that need a real port, not a transliteration

- **Threading / async model.** Today: daemon accept thread + one daemon thread
  per connection, a global storage `RLock`, thread-local WT sessions, and a
  `Condition` for change-stream `awaitData` blocking. In Rust we choose once:
  **(a)** keep thread-per-connection (`std::thread` + a blocking WT session per
  thread — closest 1:1 port, simplest), or **(b)** go async (`tokio`) with a
  bounded blocking pool for WT calls. **Recommendation: thread-per-connection to
  start** (mechanical port of the existing model, WT sessions are naturally
  thread-affine), revisit async only if a perf gauge demands it. The change-
  stream `Condition`/notify becomes a `Condvar` or a `tokio::Notify`.
- **GIL discipline.** Every `#[pyfunction]` that does real work must
  `Python::allow_threads` around the Rust body so we don't serialise on the GIL
  — this is where the multi-core win comes from. The accept loop and per-conn
  threads, once in Rust, run entirely GIL-free.
- **Geo.** `shapely` (planar predicates) → the Rust `geo`/`geo-types` crates
  (`contains`/`intersects`); `s2sphere` (2dsphere cell coverings) → the Rust
  `s2` crate; haversine/great-circle math is a handful of functions to port
  directly. Validate cell-ID encodings against golden vectors — the S2 covering
  scheme (cell + all ancestors to level 0) and the fixed-width big-endian cell
  encoding are on-disk contracts like sortkey.
- **`python-dateutil`** (variable-length `$densify` month/quarter/year — though
  CLAUDE.md notes those units are currently *rejected*) → `chrono` +
  `chronoutil`/manual month arithmetic if/when needed.
- **TLS.** Python `ssl` → `rustls` (+ `rustls-pemfile`) or `native-tls`. mTLS
  client-cert subject-DN extraction (`auth.subject_dn_from_peercert`) must be
  reproduced from the peer cert. `rustls` is the clean choice.
- **Auth.** SCRAM-SHA-1/256, MONGODB-X509 (`auth.py`, `rbac.py`, ~1,000 LOC).
  SCRAM → `hmac`/`sha2`/`pbkdf2` crates; this is fiddly but well-specified and
  the `test_auth.py` / `test_x509_auth.py` suites pin it tightly.
- **Error mapping.** The dispatch contract — *every* handler error becomes
  `{ok:0, errmsg, code, codeName}`, unknown commands return `59 CommandNotFound`
  so the connection survives — must be preserved exactly. Model as a Rust
  `CommandError { code, code_name, errmsg }` with a top-level `catch` in
  dispatch, mirroring today's `try/except` in `commands.dispatch`.

---

## 6. Phased roadmap (each phase ends green on the full pymongo suite)

The invariant: **`uv run pytest` and the six gauges stay green after every
phase.** No phase merges to `main` red. Work happens in worktrees on feature
branches per `CLAUDE.md` conventions.

### Phase 0 — De-risking spikes (no production code)
- Stand up a `crates/secantus-core` cargo workspace + maturin build producing a
  `_secantus_core` extension importable next to `secantus`.
- **Spike 1 — BSON fidelity:** differential round-trip harness (pymongo ↔ Rust
  `bson`) over fixtures; report any divergence. Go/no-go gate.
- **Spike 2 — WiredTiger FFI:** `bindgen` + `build.rs` against vendored WT;
  prove open/session/cursor/insert/scan from Rust on Linux + macOS arm64.
  Go/no-go gate for option A vs option B.
- **Spike 3 — sortkey golden vectors:** generate from Python, reproduce in Rust,
  byte-diff. Establishes the format-compat decision concretely.
- Wire `cargo test` + `cargo clippy` + `cargo fmt` into `tasks.py` and CI.

### Phase 1 — Leaf pure engines (lowest risk, highest confidence)
Port, behind the fat byte seam, in dependency order:
`paths` → `sortkey` → `collation` → `diff` → `query.matches` →
`update.apply_update` → `projection.apply_projection` → `expressions.evaluate`.
Each lands with: a Rust `#[pyfunction]`, the Python module reduced to a shim
that calls it, the **existing** `tests/test_query.py` / `test_update.py` /
`test_expressions.py` / `test_indexes.py` unchanged and green, plus Rust unit
tests mirroring the Python ones. `expressions` is the big one (~1,500 LOC, the
aggregation expression language) — budget accordingly.

### Phase 2 — Aggregation + geo
- `aggregate.apply_pipeline` (depends on Phase 1 engines) — all stages and
  `$group` accumulators. `PipelineContext` carries storage + vars; until storage
  is Rust (Phase 4), `$lookup`/`$geoNear` call back into Python storage via the
  transitional seam (§4.1-C) or are sequenced after Phase 4. **Recommendation:
  port the storage-free stages in Phase 2, defer `$lookup`/`$geoNear`
  acceleration to land with storage in Phase 4.**
- `geo` + `geo_index` (`geo`/`s2` crates) with golden-vector validation.

### Phase 3 — Cursors + change-stream projection
- `cursors.CursorRegistry` (thread-safe id→batch map, TTL pruning, injectable
  clock) → Rust.
- `changestreams.project` (resume-token encode/decode, event shaping) → Rust.
  Both are pure-ish and pin tightly against `test_change_streams.py`.

### Phase 4 — Storage (the keystone)
- Port `storage.py` onto the WT FFI (or chosen engine): collections/documents/
  indexes/index-entries tables, the index planner (`find_matching`,
  `explain_plan`, all the pickers), multikey/partial/TTL/geo indexes, oplog +
  pre-images + meta, `current_cluster_time`, retention/pruning, noop heartbeats.
- Re-home `$lookup`/`$geoNear` acceleration here (deferred from Phase 2).
- This is the largest single unit (~5k LOC) — sub-phase it by table family and
  gate each on the relevant suite (`test_storage.py`, `test_indexes.py`,
  `test_geo_index.py`, `test_change_streams.py`).

### Phase 5 — Wire, dispatch, server (close the byte path)
- Port `wire.py` (header, `OP_MSG` kind-0/1, legacy `OP_QUERY`/`OP_REPLY`,
  bounds checks) and the `commands.py` dispatch table (285 command keys), auth/
  rbac/sessions/failpoints/metrics/connreg/logbuf, and `server.py`'s accept loop
  + TLS into Rust. After this, a request never touches Python.
- `SecantusDBServer` becomes a `#[pyclass]` / thin wrapper exposing the same
  constructor kwargs, `.start()/.stop()/.address/.uri`, and context-manager
  protocol. **The public Python API is byte-for-byte unchanged** — existing user
  code and the admin app keep working.
- Optional: a `secantusdb` standalone binary `main` over the same core.

### Phase 6 — Cleanup, packaging, parity sign-off
- Delete the superseded Python modules; keep only the public-API shims.
- Replace the scikit-build-core/SWIG wheel pipeline with **maturin** (or
  scikit-build driving cargo) for cp310–cp313 across the same platform matrix
  via `cibuildwheel`. WT FFI (option A) keeps the vendored-submodule + SWIG-less
  C build; a Rust-native engine (option B) deletes most of `CMakeLists.txt`.
- Run all six driver gauges; the **pymongo "MongoDB compatibility" number must
  not regress.** Run the perf gauges — document the (expected) gains.
- Update `CLAUDE.md`, `docs/`, and `tasks/backlog.md`.

---

## 7. Testing & CI strategy

- **The pymongo + 6-driver gauges are the contract.** They run unchanged and
  must stay green/non-regressing at every phase. This is the whole reason a
  rewrite of a system this size is even sane.
- **Differential testing** against a real `mongod` (the CLAUDE.md "write a test
  that runs the same code against pymongo→SecantusDB and pymongo→real MongoDB"
  pattern) becomes the gold standard for any ambiguous behaviour uncovered
  during the port.
- **Golden vectors** for the three on-disk/byte contracts: `sortkey`, geo cell
  encodings, resume tokens — generated from the current Python, checked into the
  repo, asserted in `cargo test`.
- CI gains a Rust lane: `cargo test` + `clippy -D warnings` + `cargo fmt
  --check`, plus a maturin build smoke. The existing `ci-check` discipline
  (failures are bugs, not flakes) applies to the Rust lane too.
- Keep the Python implementation of each module alive (importable behind an env
  flag) until its Rust replacement has been green across a full CI cycle on all
  platforms — gives an instant rollback per phase.

## 8. Risk register (highest first)

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| No mature WT Rust crate; FFI/build pain across the wheel matrix | High | High | Phase-0 spike; option B (Rust-native KV) as escape hatch |
| `bson`-crate ⇄ pymongo type/key-order divergence | Medium | High | Phase-0 differential harness; gate before any engine port |
| `sortkey`/geo/token byte-format drift | Medium | High | Golden vectors; format-version bump to drop back-compat |
| SCRAM/X509 auth subtleties | Medium | Medium | `test_auth.py`/`test_x509_auth.py` pin it; `rustls`+`hmac` |
| Threading/`awaitData` blocking semantics | Medium | Medium | 1:1 thread-per-conn port first; `Condvar` mirrors `Condition` |
| Scope creep into the admin GUI | Medium | Medium | Explicitly out of scope (§9) |
| Long hybrid period / double-marshalling on hot path | Medium | Medium | Fat byte seam (§3); close the path in Phase 5 |
| Wheel/packaging regression (maturin vs scikit-build) | Medium | Medium | Keep `cibuildwheel` matrix + smoke tests; Phase 6 only |

## 9. Explicitly out of scope

- **The admin web app (`admin/**`, ~5k LOC).** It's an optional `admin` extra: a
  local FastAPI/uvicorn/pywebview GUI for browsing data, not on the request hot
  path. It will continue to run in Python and talk to the Rust core through the
  core's Python API (the same `Storage`/server surface it uses today, preserved
  by the §6 shims). Rewriting it in `axum` is a large effort with little payoff
  and can be a separate, later project if ever desired.
- Real replica sets, sharding, multi-node consistency — out of scope for the
  product, therefore out of scope here.
- New features. This is a *rewrite at parity*, not a feature release. The
  yardstick is "gauges don't regress," full stop.

## 10. Decisions needed before Phase 1 (owner: maintainer)

1. **Embeddable PyO3 extension vs standalone binary** — recommend PyO3
   extension (keep `SecantusDBServer` embeddable), optionally *also* ship a
   standalone binary. (§2)
2. **WiredTiger FFI (keep the engine) vs Rust-native KV (drop WT)** — recommend
   FFI (option A) to preserve the "same engine as MongoDB" design value; option
   B is on the table if the FFI/wheel cost proves too high in the Phase-0 spike.
   (§4.1)
3. **On-disk format: byte-compatible vs version-bump-and-break** — recommend
   version bump (ephemeral test data; no migration burden), reproducing the
   encodings faithfully so cross-type sort tests pass. (§4.2)
4. **Threading: thread-per-connection vs `tokio` async** — recommend
   thread-per-connection for the initial port. (§5)
5. **Build tool: maturin vs scikit-build-core-drives-cargo** — recommend
   maturin for the pure-Rust path; revisit if WT FFI needs CMake co-driving.
   (§6)

Items 1–3 are load-bearing for the whole plan and should be settled first.
Items 4–5 can be deferred to their respective phases.
