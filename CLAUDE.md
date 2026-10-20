# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

SecantusDB is a **surrogate single-node database**. It speaks real wire protocols on a real TCP socket over the same **WiredTiger** storage engine MongoDB ships, so an application's tests can run against it instead of standing up a real `mongod` or `postgres`. "Surrogate" rather than "fake" — these really are database servers, just intentionally scoped to single-node operation.

**Four servers, two wire protocols, one storage format.** The Rust pair is the
product. **The Python pair is DEPRECATED** (Joe, 2026-10-14: "We are focussed
on rust servers only").

**What deprecated means here:**
- **All new work goes to the Rust servers.** Do not add features, operators,
  SQL surface or performance work to the Python servers, and do not port a
  Rust fix back to Python to keep them level.
- **The Python servers are not the reference for anything.** The exemplar
  was always the real product (`mongod`, PostgreSQL; see "Design
  constraints"); the Python code remains readable prior art, nothing more.
- **Keep what exists green, do not extend it.** Their tests still run in CI
  and still have to pass; a change elsewhere that breaks one is fixed (or the
  test retired with a written reason), not ignored. When a Rust change makes
  an engine parity test fail, update the parity test's expectation for that
  case rather than changing the Python engine.
- **Compatibility with them is no longer a constraint, on-disk format
  included** (Joe, 2026-10-16: "We no longer need to worry about python
  server compatibility, we are focussed on the rust servers"). A Rust change
  may store something the Python servers cannot read, or that they would
  overwrite. Do not shape a Rust design around the Python servers, and do
  not add Python-side code to carry it. A Python test that such a change
  breaks (a cross-server store test, say) is retired with a written reason
  naming the decision, not worked around. The two RUST servers still share
  one store, and that half of the contract stands.
- **Removing them is a separate decision for Joe**: the PyPI package still
  ships them, so do not delete code, packages or docs on your own.
- `tasks/backlog-python-*.md` stay as a record; their items are not worked.

| server | binary / entry point | wire | role |
| --- | --- | --- | --- |
| **Rust MongoDB server** | `secantusd-rs` (`crates/secantusdb`) | MongoDB | **the flagship.** Ships as the `secantus-mdb` crate (crates.io) and prebuilt binaries per platform; NOT in the PyPI wheel since 2026-10-07 |
| **Rust PostgreSQL server** | `secantusd-pg` (`crates/secantus-pg`) | PostgreSQL | **the newest.** Ships as the `secantus-pg` crate (on crates.io since `0.1.0-beta.3`, 2026-10-07) and standalone binaries on the `secantusd-pg-v*` releases (Linux x86_64, macOS arm64); NOT in the PyPI wheel since 2026-10-07; builds from its own directory |
| Python MongoDB server | `SecantusDBServer` / `secantusd-py` | MongoDB | **deprecated** (2026-10-14); formerly the reference, still in the PyPI wheel |
| Python PostgreSQL server | `secantusd-py-pg` (`secantus.sql.pgserver`) | PostgreSQL | **deprecated** (2026-10-14); formerly the SQL reference, still in the PyPI wheel |

**Reach for Rust.** The marketing site already leads with the two Rust servers;
do not "restore" a Python-first framing in docs or on the site.

All four share **one on-disk format** today. Between the two Rust servers that
is a compatibility promise rather than an accident — breaking it is silent data
loss. Toward the Python servers it no longer binds (2026-10-16, above): the
cross-language bullets below record what holds now, not what must keep holding.
Be precise
about which half of it is actually pinned by a test, because they are different
claims and it is easy to cite one as evidence for the other:

- **Cross-LANGUAGE, same protocol — proven.** The Rust PG server and the Python
  PG server read and write each other's store (`tests/test_rust_pgserver_slice.py`,
  e.g. `test_python_server_reads_rust_timestamps`, which exists because getting
  it wrong is silent corruption rather than an error).
- **Cross-PROTOCOL, Python pair — proven.** A `pymongo`-written collection is
  queryable as a table, `JOIN`s and all, with no `CREATE TABLE` (`docs/sql.md`).
- **Cross-PROTOCOL, Rust pair — proven in ONE direction, and never
  concurrently** (measured 2026-09-18, pinned by three tests in
  `tests/test_rust_pgserver_slice.py`):
  - SQL → MongoDB **works**: a table written by `secantusd-pg` reads back
    through `secantusd-rs` as a collection in the database the SQL session used,
    with the `PRIMARY KEY` as `_id`.
  - MongoDB → SQL **does not**: the Rust PG server resolves names through
    `__sql_catalog__`, which only `CREATE TABLE` writes, so a `pymongo`-written
    collection is `relation "..." does not exist`. The PYTHON SQL layer samples
    an uncatalogued collection and serves it; the Rust one does not. Do not
    carry the Python behaviour across in your head — that is how this bullet
    got written wrong the first time.
  - **Not at the same time.** WiredTiger takes an exclusive file lock, so the
    second server exits rather than opening the store. "Point both at one
    directory" is a HAND-OFF, never concurrent serving — do not write it on a
    page as though both can serve at once.

The import package is `secantus`; the public Python class is `SecantusDBServer`.

**Distribution (decided 2026-10-07): the PyPI wheel carries ONLY the Python
servers; the Rust servers ship as crates.** `publish.yml` / `wheels.yml` build
with `SECANTUS_BUILD_STORAGE_ENGINE` OFF, so no `_secantus_server` /
`_secantus_storage` / `secantusd-rs` in the published wheel. The Rust MongoDB
server is the `secantus-mdb` crate (`cargo install secantus-mdb` gives
`secantusd-rs`; `secantus_mdb::Server` embeds it in a Rust test), plus the
tagged GitHub binaries. The embedded Python `RustServer` / `PgServer` handles
remain a SOURCE-BUILD tool for this repo's own tests and gauges -- do not
document them to users as something `pip install` provides. Why: the bundled
Rust pushed the project past PyPI's 10 GB quota (10.72 GB at the time).

The name was chosen to dodge brand-clash risk: an early prototype was called "fongo", a follow-on was called "fongodb", and the current name avoids both the existing "Fongo" brand and any confusion with MongoDB itself. Internal references to `fongo` or `fongodb` are stale — flag and rename to `secantus` (or `SecantusDB` for the brand form).

**In scope:** the subset of the MongoDB wire protocol that `pymongo` actually emits — connection handshake, CRUD, cursors, aggregation, findAndModify, and **change streams** (single-node, oplog-backed; collection / db / cluster scope; resume tokens; `fullDocument: "updateLookup"`; `fullDocumentBeforeChange` pre-images; `awaitData` blocking; `$changeStreamSplitLargeEvent` — an event over the BSON limit is split into fragments; on the Rust server a smaller one carries no `splitEvent`, as on `mongod` 8.2.11, while the Python server stamps `{fragment: 1, of: 1}` on every event).

**In scope on the SQL side:** the subset of the PostgreSQL wire protocol real
clients emit — the extended query protocol (Parse / Bind / Describe / Execute),
prepared statements and portals, text *and* binary formats, transactions with
savepoints and two-phase commit, `COPY`, server-side cursors, `LISTEN` / `NOTIFY`,
and the catalog tables a client introspects. The scope is set by what psycopg's
own test suite reaches for, not by a feature list.

**Explicitly out of scope:** real replica sets, sharding, multi-node consistency —
and their SQL equivalents (streaming replication, hot standby). SecantusDB advertises itself as a single-node `secantus` replica-set primary in the `hello` reply (so `pymongo`'s topology machinery accepts change streams), but the topology is fictional — there are no other members, no elections, no cross-node oplog. If a feature only makes sense in a multi-node deployment, SecantusDB does not implement it.

The audience is developers who want fast, ephemeral, single-node MongoDB or
PostgreSQL behaviour — in a test, in CI, in a container — without running the
real thing. Not a production-grade emulator.

**Performance work belongs to the Rust servers.** A benchmark that shows a
Python server slow is reporting the design, not a regression. Route latency /
throughput / concurrency work to `crates/`.

## Design constraints

- **mongod 8.x is the only version we target.** Not just the error surface —
  every behaviour. 6.0 was the reference until 2026-08-29 and is now history:
  when 8.x and 6.0 disagree, **8.x is right by definition** and matching 6.0 is
  a bug, however long-standing. Probed against 8.2.1 and 8.2.11. Do not add
  6.0-compatibility branches, and do not "fix" a difference that only appears
  on 6.0.

  `tests/test_mongod_differential.py` is the gate: it runs every supported
  operation against a real `mongod` and asserts an *exact* match. It gates on
  the **major** (`PROBED_MONGOD_MAJOR`), so any 8.x runs it and only a different
  major skips — a loud failure beats a silent skip. **The Linux CI lanes install
  `mongodb-org-server` from the 8.2 apt repo, so the gate RUNS in CI** (it used
  to skip everywhere but a dev box, which meant a green CI was no evidence at
  all for an error-surface change). It needs no separate step: the suite's
  `addopts` excludes `perf` / `online` / `slow` but not `differential`. Still
  run it locally before changing an error message — CI is the backstop, not the
  first thing you should hear it from. macOS and Windows have no server and
  still skip.

  **"Stable within a major" is a good default and not a guarantee** — two
  measured counterexamples, so probe rather than assume when a difference looks
  version-shaped:
  - `fullDocument`'s position in a change event is the same on 6.0.16 and
    8.2.1, and **moves to the end in 8.3.4** (a minor bump).
  - A wrong-type error's expected-type list is rendered in a different ORDER by
    8.2.1 and 8.2.11 (a *patch* bump). Stable per build, so it pinned a build
    rather than a behaviour; the gate now compares that list as the set it is.

  Comments saying "probed 6.0.16" without naming 8.x predate the retarget and
  were not re-verified — they record when something was measured, not what the
  server does now. Re-probe before relying on one.
- **`pymongo` is the conformance target.** Behaviour is "correct" when a `pymongo` client cannot tell SecantusDB apart from a real `mongod` for the operations it supports. When in doubt, write a test that runs the same code against `pymongo` → SecantusDB and `pymongo` → real MongoDB and assert the responses match.
- **Every server's exemplar is the REAL product it imitates, never another
  SecantusDB server.** This is one rule with two halves, and it applies to each
  server independently:

  | server | exemplar |
  | --- | --- |
  | the Rust MongoDB server | **`mongod`** |
  | the Python MongoDB server | **`mongod`** |
  | the Rust PG server | **PostgreSQL** |
  | the Python PG server | **PostgreSQL** |

  So: when you want to know what the **Rust MongoDB server** should answer,
  probe `mongod` — *not* the Python server, and not `secantus.expressions`.
  When you want to know what the **Rust PG server** should answer, probe
  PostgreSQL — not the Python PG server. The other implementation is never the
  authority, however convenient it is to reach.

  **Comparing two SecantusDB servers to each other is a DRIFT detector, not a
  correctness check.** It tells you the two moved apart; it never tells you
  which one is right, and it is silent when both are wrong together. When they
  disagree, probe the real product and move whichever side that says is wrong —
  do not reason about which looks more correct.

  This was learned the expensive way on 2026-09-08. A Rust-vs-Python comparison
  of `$asinh` showed a disagreement, the Python answer looked mathematically
  right, and the RUST side was changed to match it — moving the Rust server
  *away* from `mongod`, which underflows to a bare `0` below `1E-4966`. The
  engine-vs-engine sweep then reported "0 divergent of 384" and was cited as
  evidence of correctness; a Rust-vs-`mongod`-only sweep of 1,298 cases found
  the three shapes it had been blind to. **Report divergence counts against the
  real product, never against the other SecantusDB server.**

  A deliberate divergence from the exemplar is Joe's call, and stays as narrow
  as what he actually authorised — a decision about last-digit rounding does not
  license returning a different answer.
- **When the real product answers from state a server does not keep, model
  the real product's mechanism -- never a stand-in of our own** (Joe,
  2026-10-14: "We always choose the Postgres model"). Build that state the
  way PostgreSQL (or `mongod`) keeps it: what it persists, what survives a
  restart, what moves it. A value that is merely consistent with itself is
  not the model. The case that set the rule: `age(xid)`. This server had no
  32-bit xid counter, and the first version froze the catalog at each run's
  first xid, so `age(datfrozenxid)` counted from startup and reset on every
  restart. It was replaced before merging by PostgreSQL's own model: one
  persisted counter that runs on across restarts, and `datfrozenxid` fixed
  at 722 where `initdb` leaves it (`crates/secantus-pg/src/xids.rs`). When
  the real mechanism is out of reach, refuse by name (0A000) and put it in
  the backlog; a plausible stand-in is a decision for Joe, never the
  default.
- **Wire-protocol fidelity over feature completeness.** Prefer returning a faithful "command not supported" error over a half-implemented feature that silently diverges from real server behaviour.
- **Ease of use for the beginning programmer:** starting a server in a test should be one or two lines, with no external processes to manage.

## Never ignore or discount an error — this is a database

SecantusDB stores data. In a database, an error is a **correctness and durability signal**, never noise to step over. Every error, panic, warning, or failed assertion — especially from the storage engine (WiredTiger) — is treated as a real bug until proven otherwise by a root-cause diagnosis.

- **Never dismiss, deselect, suppress, or "retry past" an error to make output green.** A `WT_PANIC`, a checkpoint failure, a `WT_ROLLBACK`, a "the system must restart", a corrupted-read, a swallowed exception in a write path — each means data may be wrong or lost. Reproduce it (a focused harness like `invoke rust-stress`), find the actual cause, and fix that cause.
- **"Flaky", "environmental", "only under load", "only in parallel" are descriptions of a bug, not excuses to ignore one.** A storage engine that panics under stress is broken even if a single-threaded test passes — stress is exactly when databases must hold. Diagnose the race / resource / lifecycle issue and fix it.
- **A test failing is the system telling you something true.** Before reaching for a skip/xfail/deselect, prove the failure is a test artifact unrelated to data integrity — and even then prefer fixing the test over hiding it. Deselecting a storage test to get a clean run is how silent data loss ships.
- **Surface errors faithfully.** Don't downgrade a storage error to a generic message, don't `let _ =` away a `Result` on a write/commit/close path, and don't report "done" while an error was logged. If a write, checkpoint, or connection close errored, that is the headline, not a footnote.
- **A test that does not COMPLETE is a failure, not a skip — and never a pass.**
  A hang, a timeout kill, a crashed runner, a suite that stops part-way: each
  means the thing it was checking went unchecked, and the result is unknown, not
  good. Never let a run that did not account for every test it started be
  reported as a rate, a pass, or a green tick.

  This is not hypothetical. The Go gauge published **100.0%** for months over a
  run that a hung DNS test killed at 476 of 481 — `go test` panicking on
  `-timeout` emits no terminal event for the tests still in flight, so the
  summariser divided what it could see and got a perfect score. **A truncated
  run looks BETTER the more tests go missing**, which is exactly why it survives
  review. Before believing any suite number, compare tests STARTED against tests
  that REPORTED a result.

  And fix the cause, not the symptom. Two sessions treated this one without
  fixing it: the first raised the timeout from 10 minutes to 30 so the kill
  would stop "corrupting the signal" (cost tripled, population still capped),
  the second made the report refuse to print a rate (honest, but still no
  number). The fix was to stop the test hanging.
- **Never skip or hide a failure to get a clean run.** No `-k`, `--deselect`,
  `-skip`, `xfail`, try/except-pass, or quietly narrowed include list to make a
  red thing green. If something genuinely does not belong in a gauge's scope, it
  is **excluded in the curated include list with a written reason naming what is
  lost** — which is a scope decision anyone can read and argue with, not a
  disappearance. A skip whose comment says "flaky" or "fails in CI" is the
  smell; a skip whose comment says what it measured and why that is out of scope
  is a decision.

## Architecture

The layer map, the WiredTiger table layout the servers share, the index
planner (equality / range / compound / multikey / partial / sparse / TTL /
geo), the oplog and change streams were first built in the Python server and
are described in the **`python-servers` skill**. Read it before changing the
storage layout or index planning on either side: the Rust storage ported those
helpers, and the parity suites do not cover them. The Rust crates mirror the
layers (see "Engines" below).

**The Rust MongoDB server reproduces mongod's `explain` (2026-10-12)** -- `indexBounds`, the `IDHACK` / `EXPRESS_IXSCAN` / `COUNT_SCAN` / `DISTINCT_SCAN` / `PROJECTION_COVERED` executors and every command's stages, because they turned out to follow fixed rules whenever only one index can serve the query: its `explain` (`crates/secantus-commands/src/explain/`) derives the plan from mongod's planning rules — bounds, residual filters, sort and covering, and the `count` / `distinct` / write stages — and derives `executionStats` by walking that plan over the documents. What it still does not reproduce is the part that really is mongod's trial of candidate plans: `rejectedPlans`, the plan cache, and the choice between several usable indexes (a fixed order of preference stands in, and agreed with mongod on every measured case). It reports the plan mongod runs, which is not always how the Rust storage router reads the documents (`tasks/backlog-rust-mongodb.md` 7.075). Sweep: `tools/probes/explain_commands.py`, 786 replies. **Since 2026-10-13 the same plan sets the ORDER the Rust server returns documents in** (`explain/order.rs`, `tasks/backlog-rust-mongodb.md` 7.080): the router says only which documents match, and the plan says in what order mongod reads them, which is also which document a `limit: 1` delete or an update without `multi` changes. A sort is still the store's own; the plan decides how documents that compare equal are left. For a sort the predicate's index does not return, the planner counts mongod's trial of the two candidate plans (`plan::walk_finishes_first`) on a collection of up to 2,000 documents. Sweep: `tools/probes/find_order.py`, 6,531 replies. When a change touches the router or the planner, run it: a router change can no longer move the order, and a planner change now can.

### Type-mapping strategy (the critical decision)

Documents are stored as **opaque BSON blobs**. All filtering, projection, sorting, and updates happen in Python after `bson.decode`. The storage layer never inspects document content. This is deliberate: SecantusDB's whole point is that `pymongo` cannot tell us apart from `mongod`, and any lossy intermediate representation (JSON, native column types, etc.) would break that for ObjectId / Decimal128 / int32-vs-int64 / Date-with-tz / Binary / Regex.

When secondary indexes land they will be WT indexes over typed sort-key columns derived from BSON values — not JSON, not coerced numerics.

### Engines: FOUR SEPARATE SERVERS across two wire protocols

**Direction (current):** SecantusDB ships **completely separate servers**, and a
user runs **one of them** — never a per-operator/per-`Storage` selection inside
one request path:

- **The Rust MongoDB server** (`secantusd-rs`, `crates/secantusdb`) — a whole,
  self-contained Rust server (its own wire / dispatch / cursors / accept loop over
  the pure-Rust `secantus-core` + `secantus-storage` + `secantus-wt` crates). No
  Python in the request path. Its Python ergonomic is a **thin embedded lifecycle
  handle** (`start`/`stop`/`address`) — the accept loop runs on a GIL-released Rust
  thread in-process and `pymongo` connects over real TCP; Python is only the
  launcher, never an operator. **This is the server the project leads with.**
- **The Rust PostgreSQL server** (`secantusd-pg`, `crates/secantus-pg`) —
  the PostgreSQL wire protocol over the same `secantus-storage`. SQL is parsed
  by **`libpg_query`** (the real PostgreSQL grammar via `pg_query`), lowered by
  `secantus-pgplan` to the MQL filters `secantus-core` already evaluates,
  against the catalog in `secantus-pgcatalog`. It builds from **its own
  directory** (`./inv rust-pgserver-build`), because it links WiredTiger and
  is excluded from the clean workspace. **Before any work on it, read the
  `rust-pg-server` skill** (`.claude/skills/rust-pg-server/SKILL.md`): how to
  build, test and measure it, the reference servers (PostgreSQL 15.19 on port
  5415, 14 as the default cluster), where each kind of change goes, and the
  rules earlier batches paid for. Open work is `tasks/backlog-rust-postgres.md`;
  the record of what landed when is git history and `changelog.d/`.
- **The Python MongoDB server** — the *original* `SecantusDBServer` (pure-Python
  `server` / `wire` / `commands` / `Storage` / operator engines). No Rust in the
  request path.
- **The Python PostgreSQL server** (`secantus.sql.pgserver`) — the PostgreSQL wire
  over the same Python `Storage`. Deprecated: the Rust PostgreSQL server is its
  replacement and now the more complete SQL surface.

#### Versioning: three independent version lines

- **The Python package**: `pyproject.toml` `version` + `src/secantus/__init__.py`
  `__version__` (`0.5.2bN`, PEP 440), set by `invoke release-prepare`.
- **The Rust MongoDB server**: the `version` of the thirteen MongoDB-side
  `crates/*/Cargo.toml`, in **lockstep** (`0.MAJOR.PATCH-beta.N`). Bumping the
  patch (or minor/major) resets the beta label to 0 (`0.5.2-beta.20` →
  `0.5.3-beta.0`). Surfaced as `buildInfo.secantusVersion`, `secantusd-rs
  --version` and `RustServer.version` (from `secantus_server::VERSION`).
- **The Rust PostgreSQL server**: `secantus-pgcatalog` / `secantus-pgplan` /
  `secantus-pg`, a line of their own that is NOT bumped with the MongoDB server.

Bump a Rust line with `./inv rust-version-bump --to <version> [--line pg]`,
never a blanket substitution across `crates/`, which would fold the PG crates
into the MongoDB line. Release mechanics: the `secantusdb-release` skill.

**No version bumps in feature PRs — either server.** Both the Python and the Rust
version are assigned at release time, so a feature PR touches neither `version`
line. This is the whole point: an in-flight PR that bumped a version picked a
number that was stale by merge time, so any two concurrent PRs collided on that
one line (Python) or on the thirteen `Cargo.toml` (Rust). Leaving both to the
release makes concurrent feature PRs — Python-only, Rust-only, or dual-server —
independent. Describe the change in a `changelog.d/` fragment (see the
Documentation / Conventions sections); the release stamps the version.

**The authoritative plan is `tasks/rust-server-plan.md`.** It supersedes the
earlier *in-process selectable-engine* model (`SECANTUS_ENGINE` process-wide
selection / the `secantus.engine` per-component shims / the "5e Python `Storage`
adapter + `EngineFallback`"). Read it before doing Rust-side work.

The Rust side is a Cargo workspace under `crates/`: `secantus-core` (pure-Rust
engines + geo + aggregation, **no PyO3**), `secantus-storage` (the full WT-backed
`Storage`, pure-Rust), `secantus-wt` (the WT FFI), plus thin PyO3 binding crates
(`secantus-core-py` → the `_secantus_core` abi3 extension). The PyO3-free split is
exactly what lets the Rust server / standalone `secantusd-rs` binary reuse the
engines.

- **Parity suites are a DRIFT detector, not a reference.** Each pure Rust
  engine is pinned to its Python counterpart (`tests/test_rust_*_parity.py`).
  A failure says the engines moved apart, never which is right: probe
  `mongod`, and when the Rust change was correct, update the parity
  EXPECTATION for that case; never change the Python engine to match. The
  suites do not cover the storage layer. The transitional in-process engine
  selection (`src/secantus/engine.py`) and the rest of this history are in the
  `python-servers` skill.

## Tooling

- **Ad-hoc reproducers and cross-driver smokes use `127.0.0.1:27018`** (mongod's standard "alternate" port — out of the way of any real `mongod` running on `27017`, but predictable enough that test-failure messages cite a verbatim address you can hit by hand). Test scripts that need to talk to SecantusDB ad hoc should try `127.0.0.1:27018` first. (Conformance gauges use kernel-assigned ephemeral ports per run — see `/conformance-gauges`.)
- Python 3.12 pinned via `.python-version`. Managed with `uv`. Always invoke Python via `uv run python -m ...` so `pyenv` doesn't intercept.
- Build/admin tasks: `tasks.py` (`invoke`). `invoke test`, `invoke lint`, `invoke fmt`, `invoke docs`, `invoke serve`.
- **Admin-UI screenshots are generated, never hand-captured.** `invoke admin-screenshots` (`scripts/admin_screenshots.py`, `screenshots` extra + `playwright install chromium`) boots a throwaway server on `127.0.0.1:27018`, seeds a fictional dataset, drives all 22 admin pages with Playwright, scrubs machine-specific strings from the DOM, and writes `docs/screenshots/`. That directory is the single source for all three surfaces that show the shots: Sphinx (`docs/admin.md`) references it, the README links it on raw.githubusercontent, and the Pelican build copies four of them into the theme at build time (`website/tasks.py` `SITE_SCREENSHOTS` — the theme's `static/img/` is an untracked build mirror). **Regenerate on every release** — `tests/test_docs_screenshots.py` checks that every page has an image and every image is referenced, but nothing can detect a *stale* image, so the `secantusdb-release` skill makes regeneration a mandatory pre-flight step. The run is also the admin UI's only browser-level smoke test: it exits non-zero on any JS error and warns on any page shot in an empty state (that's how four dead-dashboard bugs were found). When iterating from a worktree whose venv belongs to another checkout, pass `--from-checkout .` so it renders *your* templates and static assets rather than the installed package's.
- `pytest` with `pytest-xdist` parallel by default (`addopts = "-n auto"`).
  Tests must use `port=0` and a unique `storage_path` per test (`tmp_path`).
  The default suite runs real on-disk WiredTiger in **fast test-storage mode**
  (`SECANTUS_TEST_FAST_STORAGE=1`: no close-time checkpoint), so **checkpoint
  durability is NOT covered locally**; CI's `SECANTUS_FORCE_DURABLE=1` lane
  covers it, and so does running that variable locally. Details:
  `build-and-test`.
- **Never use a mock storage — always the real `Storage`.** The legacy `FakeStorage` mock (`tests/sqlfake.py`) has been **removed**: every SQL / PG-server test now drives the real WiredTiger-backed `Storage`, and no new mock should be reintroduced. A mock lets a test pass while diverging from what WiredTiger actually does (row/type round-trips, transactions, cross-session visibility, persistence) — the migration off it surfaced several real bugs the mock had masked (a swallowed tz-aware/naive datetime comparison, a `bytea` equality mismatch, a Describe/Execute uncommitted-DDL protocol crash, timestamptz text-render). Build a real `Storage(str(tmp_path))` (close it in the fixture teardown — `try: yield s finally: s.close()`), seed it via `run_sql` with an unrestricted `Session`, and drive the SQL/PG-server code against that. This is a database — tests prove behaviour against the real engine or they prove nothing.
- **Driver-conformance gauges** -- thirteen MongoDB-driver gauges and three SQL
  ones run **unmodified** upstream test suites against SecantusDB (invocation,
  toolchains and quirks: `/conformance-gauges`). **Keep `invoke validate-all
  --jobs` at 4 or fewer** (1 for a committable number): beyond that CPU
  contention flakes timing-sensitive tests. The C++ gauge binds port 27017.
- **Run full gauges in a sub-agent**, not the main session, and use `invoke
  validate-one <nodeid> --server <python|rust>` to iterate on one test. Always
  pass `--server`; see the `measuring` skill.
- **Long runs go through `scripts/detached_run.py`**, not a bare background
  shell, which a supervisor reaping its process group kills mid-run
  (`build-and-test` has the commands).
- **The Rust servers are built with PGO** on top of LTO. The wheel extension's
  profile is committed at `crates/pgo/_secantus_server.profdata.tar.gz` (do
  not delete it; refresh with `invoke rust-pgo-refresh`); the release binaries
  collect theirs at release time. Details: `build-and-test`.
- WiredTiger is **vendored** at `vendor/wiredtiger` (mongodb-7.0.33). Fix
  WiredTiger-side problems by extending a `cmake/patch_wt_*.py` patcher, never by
  editing the submodule; two patchers change behaviour (group commit under
  `dsync` / `fsync`), and three builds must apply the same list. Details, and
  the wheel matrix: the `build-and-test` skill.
- **Sync with `./inv sync`, never a bare `uv sync`**: `uv sync` does not
  rebuild the `secantus-core` path dependency, and a sync without the `rust`
  extra uncollects ~1,700 parity tests while still exiting 0 (`batch-worktree`).
- To run a single test serially: `uv run python -m pytest -n0 tests/path::test_name`. The `-p no:xdist` form fails because `addopts` still injects `-n auto`.
- Sphinx docs in `docs/` (Markdown via `myst-parser`, furo theme). Built with `-W` (warnings-as-errors). `invoke docs` to build, `invoke docs-serve` to preview.
- PyPI publishing is OIDC-only via `.github/workflows/publish.yml` on `vX.Y.Z` tags. The workflow refuses to publish if the tag doesn't match `pyproject.toml`'s version. Never run `uv publish` / `twine upload` manually.
- The on-disk repo path is `/Users/jdrumgoole/GIT/SecantusDB`. The **import package** is `secantus` (`src/secantus/`, `import secantus`); the **distribution / PyPI name** is `SecantusDB` (`pip install SecantusDB`, normalized by PyPI to `secantusdb`). These are deliberately different — `pip install secantus` is a 404. The release tooling uses the distribution name (`tasks.py` polls `pypi.org/pypi/SecantusDB/json`; `publish.yml`'s trusted-publisher environment binds `pypi.org/p/secantusdb`).

## Releases and website

Both procedures are managed by skills — they auto-fire on the relevant trigger phrases. To inspect or invoke manually: `/secantusdb-release` and `/secantusdb-website`. The skill files are under `~/.claude/skills/`; treat them as the source of truth and edit them there, not here.

- **Release-time benchmark refresh**: `invoke release-benchmark`, after the
  version bump lands on `main`, so `docs/benchmark.md` measures the released
  build (`secantusdb-release`, `bench/DO_CLUSTER.md`).
- **`secantusdb-release`** — the two-phase pipeline (`release-prepare` once in foreground, `release-finalize` in a foreground retry loop), the sub-agent contract, foreground-only constraints, the 8-step pipeline (pre-flight → pytest → perf gates → changelog collation → bump → tag/push → GitHub Release → PyPI workflow + listing), and the hard prohibitions against manual `git tag`/`uv publish`. Docs are **self-hosted at secantusdb.com/docs/** (main tree) and **/docs/rust/** (Rust server) — they deploy with the post-release website publish, not the release pipeline; the readthedocs.io copies are legacy, kept alive with a moved banner.
- **A Rust server release ends at the live site, not at the GitHub Release**:
  bump the pinned tag in `website/pelicanconf.py`, re-read the server's page,
  write the blog post by hand, deploy. Steps: `secantusdb-release`.
- **`secantusdb-website`** — **website content lives on `main`** (the retired `SecantusDB-website` / `website-dev` worktree pattern is gone; Pelican builds the live site from `main`). Edit `website/` on a short feature branch and land via PR (website-only commits skip the full pytest run — only `website/`, this `CLAUDE.md`, and `.gitignore` qualify), then `invoke deploy` from `main` to build + `aws s3 sync` + invalidate CloudFront. Also covers the driver-panel regeneration and the per-release blog-post template (descriptive title + prose body + link bar — never a stub linking out to GitHub).

  **The site leads with the two Rust servers.** Its pages are templates,
  its performance numbers and driver panels are generated (never hand-edit
  them; a blank panel means a gauge did not finish), and a
  `validation_summary/` change is not website-only. The traps are in the
  `secantusdb-website` skill.

## Backlog of stubs and stopgaps

The backlog is the canonical list of commands that are stubbed, features with simplified implementations, and work explicitly deferred from a slice. It is **four files, one per server** (split from the single `tasks/backlog.md` on 2026-10-14, which is now their index):

| file | server |
| --- | --- |
| `tasks/backlog-rust-mongodb.md` | the Rust MongoDB server, and the Rust storage both Rust servers use; also CI / build / release items no one server owns |
| `tasks/backlog-rust-postgres.md` | the Rust PostgreSQL server |
| `tasks/backlog-python-mongodb.md` | the Python MongoDB server and the admin UI — **deprecated: a record, not worked** |
| `tasks/backlog-python-postgres.md` | the Python PostgreSQL / SQL server — **deprecated: a record, not worked** |

**Update the right file whenever you stub something, defer a slice, or discover a limitation; an item about more than one server goes in each file it concerns** (do not add new items to the two deprecated Python files). When you fix an item, delete its lines** — the files hold open work only, and git history and `changelog.d/` hold what is finished (`tests/test_backlog_markers.py` fails on a ticked `- [x]` item). Future sessions should treat these files as load-bearing — they are the only honest record of where SecantusDB's behaviour diverges from the real products. A section number (`7.077`, `§3.1`) means the same section it did in the single file.

**Every Rust item is a failing test with prose attached.** Each carries a permanent `Backlog-ID:` (`RM-nnn` / `RP-nnn`) and a `Verify:` line naming a gap test under `tests/backlog/` -- asserting the real product's answer, so it fails while the item is open -- and `python tools/backlog.py verify` runs them all (OPEN / FIXED? / BROKEN / UNCHECKED). A fix turns its gap test into the regression test and the `changelog.d/` fragment names both (`Fixes RM-142 (tests/...::test_x)`), so every fix stays traceable to the test that proves it. **The `backlog` skill is the protocol: read it before filing, working or closing an item.**

## Finding bugs: run it against the reference server, don't reason about the source

Every bug found in the 2026-08 sweeps came from **executing behaviour and
comparing against a real server**, not from reading code or the backlog, and
estimates from reading code have been wrong in BOTH directions (a one-line fix
filed as a 16-site refactor; a "planner slice" that already existed).
**Reproduce an item before working it, and size it from a probe.**

**Read the `differential-probe` skill before probing.** It has the reference
servers on this box (mongod 8.2.11 on `PATH`, 6.0.16 and 8.3.4 by Cellar path;
PostgreSQL 15.19 on port 5415 and 14), the probe recipe, the four ways a probe
lies, and the bug shapes worth grepping for (user paths as dict keys, comments
citing the other engine, Python `==`/`hash()` for a BSON semantic, serde BSON
decodes in the Rust servers, missing conflated with null). Cite the exact
`mongod --version` you measured: a patch bump has changed an error message.

The standing gate is `tests/test_mongod_differential.py` (`pytest -m
differential`): a probe that finds something belongs there, or the next
session re-finds it.

**Parity is not correctness.** The parity suites pin the two engines to each
other, so they are equally satisfied by both being wrong (`$bucket`,
`$densify`, `$stdDev*`), and a hand-run engine-vs-engine sweep is the same
trap with nothing to warn you. When you are establishing what is correct,
leave the other SecantusDB server out of the comparison entirely.

**Before quoting any number** -- a gauge rate, a suite count, a benchmark, a
corpus divergence count -- read the `measuring` skill.

## CI is load-bearing — failures are serious bugs, not flakes

After every push (to a feature branch via PR — the default — or to `main`), check the corresponding GitHub Actions run and **resolve any failures before moving on**. CI is the source of truth — it catches cold-cache races, cross-platform / cross-Python drift, and missing-CI-extra gaps that local-only testing misses. Procedure (`gh run list`/`watch`/`view --log-failed`), the docs-only `paths-ignore` exception (markdown / LICENSE / `docs/**` commits skip CI by design), and the recurring-failure-pattern catalog (xdist install-state races, per-platform sysconf, `pytest-subtests` outcome accounting) all live in `/ci-check`. Add new patterns to that skill as they show up. **Watch the PR's CI run, not `main`'s** — with the branch+PR flow (see "Conventions" below) each branch is its own CI lane, so a parallel session's push can no longer cancel your run; a `cancelled` conclusion now means *you* superseded it with a newer push to the same branch.

## Parallel sessions and subagents: a machine-wide budget

This Mac has **12 cores and 32 GB**, and Joe runs **two sessions at once**.
The limit is the machine, not the session, so it is shared:

- **At most 4 agents that build or test, across both sessions: 2 per
  session.** Each compiles its own worktree (a WiredTiger-linked
  `cargo build` is several minutes and about 1.3 GB of `target/` for the
  server binary alone, more as the other crates build).
- **Every one of them sets `CARGO_BUILD_JOBS=3`** (and `cargo test --
  --test-threads=4`). Cargo's default is one job per core, so each agent
  alone would take the whole machine: 4 agents x 3 jobs = 12, one job per
  core.
- **Agents that only read, search or probe a running server** (no cargo, no
  pytest) are cheap: one or two more per session on top of the four.
- Count what is ALREADY running before spawning: the other session's agents
  are invisible to you, so ask, or read `uptime`. A load average well above
  12 means the budget is spent.

Measured 2026-10-15: eight agents building at once (five in one session,
three in the other), each at cargo's default 12 jobs, held the load average
at **66 on 12 cores** with the memory compressor engaged. Builds that take
minutes alone took several times longer, and a timing-sensitive test fails
under that contention for no reason of its own.

The same arithmetic is why `invoke validate-all --jobs` stays at 4 or fewer
(Tooling, above): do not run a gauge sweep beside building agents at all.

## Conventions for changes here

- **Never commit build output or any other generated binary.** No cargo
  `target*/` directory, no `.so` / `.dylib` / `.a` / `.rlib` / `.o`, no
  executable, no wheel or sdist, no `.profraw`, no core dump, no database
  store. Source and small hand-made fixtures only. On 2026-09-30 a
  `git add -A` swept `crates/secantus-storage/target-dev/` into a feature
  commit: 6,875 files and 1.77 GB. Nobody noticed for a week, until PyPI
  refused a 573 MB sdist, and git history had to be rewritten to get the
  repository back from 513 MB to about 70.
  - **Read `git status --short` before every `git add -A`**, and stage by
    path when anything in it is not a file you wrote. An unfamiliar
    directory in that list is the warning.
  - A new build directory gets a `.gitignore` line in the same change that
    creates it. `CARGO_TARGET_DIR=<anything>` inside the repository is how
    this one got past the existing ignore rules.
  - `tests/test_no_tracked_build_output.py` fails on a tracked cargo target
    directory or any tracked file over 8 MB. Run it when in doubt; do not
    raise its limit to get something in.
  - The one binary that IS committed on purpose is the PGO profile,
    `crates/pgo/_secantus_server.profdata.tar.gz` (see Tooling). Adding
    another needs Joe's say-so.
- **One session works on ONE Rust server: the PostgreSQL server or the
  MongoDB server, never both** (Joe, 2026-10-14). The two share
  `secantus-storage` and the WiredTiger build, and parallel sessions on each
  side kept colliding in that shared code and in each other's branches. Pick
  the side at the start of a session (`tasks/backlog-rust-postgres.md` or
  `tasks/backlog-rust-mongodb.md`) and stay on it; an item on the other side
  goes to a session working that side. A change the item needs in the shared
  storage crates is fine -- say in its PR that the other server links it.
- **Claim a backlog item before working it — the claim is a PUSHED branch.**
  The same item has been built twice by parallel sessions. Check
  `git ls-remote --heads origin` and `gh pr list --state open` first,
  **reproduce the item** (the backlog lags reality in both directions), then
  create the branch and push it before the first commit. Never delete or
  force-push another session's claim branch. Details: `batch-worktree`.
- **Batch several slices into ONE branch and PR — do not run the full suite per
  small feature.** The suite is ~6,000 tests and 12–13 minutes; running it after
  every one-file change makes the ceremony cost dwarf the work. Nine
  single-slice PRs in one stretch (each with its own worktree, full suite, lint,
  changelog fragment, backlog edit, PR, CI watch, merge, teardown) burned a
  session's budget while the task board barely moved. Instead:
  - while iterating, run only the **targeted** tests (`uv run python -m pytest
    -n0 tests/test_x.py`) plus whatever gauge / corpus command proves the slice;
  - accumulate related slices on one branch;
  - run the **full suite once** at the end, before the batch's commit — this
    still satisfies "run the full test suite before committing", because the
    batch is what gets committed;
  - one PR, one CI watch, one merge, one teardown per batch.

  An incidental discovery (a flaky test, a CI annoyance, a stray temp dir) gets
  folded into the current batch or written down — it does not get its own full
  cycle.
- **Major features and non-trivial updates go in a git worktree on a feature branch — never directly on `main`.** New CRUD operators, aggregation stages, storage-layer changes, wire-protocol additions, indexing work, and similar multi-file changes all qualify. Trivial one-file tweaks (typo fixes, single-line config edits) can stay on `main`. Create a worktree alongside the repo: `git worktree add ../SecantusDB-<branch> -b <branch>`. Develop and run the full test suite (and `./inv rust-gate` for Rust-server work) there.
- **Pin a worktree to a commit for any timing / performance measurement**
  (`git worktree add --detach ../SecantusDB-measure "$(git rev-parse
  origin/main)"`): parallel sessions move `main` mid-run. The `measuring`
  skill has the rest of what makes a number trustworthy.
- **Land via branch push + PR, not `git push HEAD:main`.** Push the feature branch (`git push -u origin <branch>`) and open a PR (`gh pr create --base main`); let CI run on the PR, then merge when green. **Do not push straight to `main` from a session** — every session pushing the same ref (`refs/heads/main`) lands them in the same CI concurrency group, so a newer push cancels the older session's in-flight run (and forces constant rebases). One feature branch per session = one CI lane per session; distinct refs never cancel each other. After merge, tear the branch down — see the next bullet. (`test.yml` / `wheels.yml` / `rust-wheels.yml` are keyed `concurrency: ${{ github.workflow }}-${{ github.ref }}`, `cancel-in-progress: true` — per-ref, so the cancel only bites within a single branch, which is what you want.)
- **Cleaning up is part of finishing the work.** Tear down the branch and
  worktree in the SAME session that merges (worktree before branch; `-D` only
  after `gh pr view <N> --json state` says `MERGED`), and never remove a
  worktree, branch or stash you did not create in this conversation. The
  sequence and its traps: `batch-worktree`. Everything else a session leaves
  running: `session-cleanup`; closing a whole session: `close-session`;
  opening one: `start-session`.
- **Record the changelog with a fragment, not by editing `docs/changelog.md`.** A feature PR adds one file `changelog.d/<short-slug>.md` holding a single entry (a `###` headline + prose lede + `#### Added` / `#### Fixed` / … sections, no version number or `##` header) — see `changelog.d/README.md`. **Do not edit `docs/changelog.md` directly in a feature PR** and **do not bump the Python `version`** (see the Versioning section): those two shared lines are what made every pair of concurrent PRs conflict. New fragment files never collide, so concurrent sessions stay independent. At release, `invoke changelog-collate` (run automatically by `release-prepare`) folds the fragments into `## [Unreleased]` and deletes them; the normal promote-to-a-dated-section step is unchanged.
- New CRUD operators or aggregation stages should land with both a unit test (in `tests/test_query.py` / `tests/test_update.py` / `tests/test_aggregate.py` / `tests/test_expressions.py`) and a `pymongo`-driven integration test in `tests/test_crud.py`. The integration test is the conformance proof; the unit test pins the semantics.
- Layer boundaries to defend: the wire layer never knows about commands; the command layer never knows about SQL; pure operator engines (`query`, `update`, `expressions`, `aggregate`, `projection`) take only docs in and out, no I/O.
- Errors raised inside command handlers are caught by `dispatch` and turned into `{ok: 0, errmsg, code, codeName}`. Don't leak Python tracebacks to the wire.
