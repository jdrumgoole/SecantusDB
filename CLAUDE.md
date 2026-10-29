# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

SecantusDB is a **surrogate single-node database**. It speaks real wire protocols on a real TCP socket over the same **WiredTiger** storage engine MongoDB ships, so an application's tests can run against it instead of standing up a real `mongod` or `postgres`. "Surrogate" rather than "fake" — these really are database servers, just intentionally scoped to single-node operation.

**Four servers, two wire protocols, one storage format.** The Rust pair is the
product. **The Python pair is FULLY DEPRECATED** (Joe, 2026-10-14: "We are
focussed on rust servers only"; 2026-10-20: stop publishing to PyPI, and take
the Python servers out of CI and the release process).

**What fully deprecated means here:**
- **All new work goes to the Rust servers.** Do not add features, operators,
  SQL surface or performance work to the Python servers, and do not port a
  Rust fix back to Python to keep them level.
- **The Python servers are not the reference for anything.** The exemplar
  was always the real product (`mongod`, PostgreSQL; see "Design
  constraints"); the Python code remains readable prior art, nothing more.
- **Nothing publishes them, and CI does not test them** (2026-10-20). A
  Python-server test a change breaks is not your problem to fix. pytest stays
  the test runner, including for the Rust servers' Python test files.
- **Do not delete the `secantus` package.** Every Rust job builds WiredTiger
  through its CMake build, and several Rust tests import its `Storage` / SQL
  layer. Removing it is a later decision for Joe.
- **Compatibility with them, on-disk format included, is no longer a
  constraint** (2026-10-16). Do not shape a Rust design around them. A Rust
  test that only checks a Python hand-off and that breaks is retired with a
  written reason naming the decision, not worked around.
- Which CI jobs and tests these bullets refer to: the `python-servers` skill.
- `tasks/backlog-python-*.md` stay as a record; their items are not worked.

| server | binary / entry point | wire | role |
| --- | --- | --- | --- |
| **Rust MongoDB server** | `secantusd-rs` (`crates/secantusdb`) | MongoDB | **the flagship.** Ships as the `secantus-mdb` crate (crates.io) and prebuilt binaries per platform |
| **Rust PostgreSQL server** | `secantusd-pg` (`crates/secantus-pg`) | PostgreSQL | **the newest.** Ships as the `secantus-pg` crate (on crates.io since `0.1.0-beta.3`, 2026-10-07) and standalone binaries on the `secantusd-pg-v*` releases (Linux x86_64, macOS arm64); builds from its own directory |
| Python MongoDB server | `SecantusDBServer` / `secantusd-py` | MongoDB | **fully deprecated** (2026-10-20): not tested in CI, not published |
| Python PostgreSQL server | `secantusd-py-pg` (`secantus.sql.pgserver`) | PostgreSQL | **fully deprecated** (2026-10-20): not tested in CI, not published |

**Reach for Rust.** The marketing site already leads with the two Rust servers;
do not "restore" a Python-first framing in docs or on the site.

**One on-disk format.** Between the two RUST servers it is a compatibility
promise -- breaking it is silent data loss. Cross-protocol between them is
proven in ONE direction and never concurrently (measured 2026-09-18, pinned in
`tests/test_rust_pgserver_slice.py`):
  - SQL -> MongoDB **works**: a `secantusd-pg` table reads back through
    `secantusd-rs` as a collection, with the `PRIMARY KEY` as `_id`.
  - MongoDB -> SQL **does not**: the Rust PG server resolves names through
    `__sql_catalog__`, which only `CREATE TABLE` writes. The Python SQL layer's
    sampling of uncatalogued collections does NOT carry across.
  - **Not at the same time.** WiredTiger's exclusive file lock makes the second
    server exit. "Point both at one directory" is a HAND-OFF, never concurrent
    serving -- do not write it on a page as though both can serve at once.

The import package is `secantus`; the public Python class is `SecantusDBServer`.

**Distribution: crates and tagged binaries only; nothing goes to PyPI**
(2026-10-20): the `secantus-mdb` crate (`secantusd-rs`, `secantus_mdb::Server`)
plus `secantusdb-v*` binaries, and the `secantus-pg` crate plus `secantusd-pg-v*`
binaries. The embedded Python `RustServer` / `PgServer` handles are a
SOURCE-BUILD tool for this repo's tests -- never document them to users as
something `pip install` provides.

Internal references to `fongo` or `fongodb` (earlier names, dropped to avoid brand clashes) are stale -- flag and rename to `secantus` (or `SecantusDB` for the brand form).

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

  `tests/test_mongod_differential.py` runs every supported operation against a
  real `mongod` and asserts an *exact* match, and gates on the **major**
  (`PROBED_MONGOD_MAJOR`). **It targets the PYTHON server** (its
  `secantus_uri` fixture starts `SecantusDBServer`), so since 2026-10-20 it
  does not run in CI and is no evidence about the Rust server. For the Rust
  MongoDB server the evidence is what CI runs against a live mongod 8.2 on
  every PR (since 2026-10-21, the storage-engine job): the backlog's gap and
  `backlog_fixed` regression tests (`tools/backlog.py verify --file
  backlog-rust-mongodb.md --regressions`) and `tests/mongod_live/`; plus
  probes (the `differential-probe` skill) and the sweeps under
  `tools/probes/`, which no workflow runs.

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

  (The 2026-09-08 `$asinh` incident that set this rule: `differential-probe`.)

  A deliberate divergence from the exemplar is Joe's call, and stays as narrow
  as what he actually authorised — a decision about last-digit rounding does not
  license returning a different answer.
- **When the real product answers from state a server does not keep, model
  the real product's mechanism -- never a stand-in of our own** (Joe,
  2026-10-14: "We always choose the Postgres model"). Build that state the
  way PostgreSQL (or `mongod`) keeps it: what it persists, what survives a
  restart, what moves it. A value that is merely consistent with itself is
  not the model. (The `age(xid)` case that set the rule: `rust-pg-server`.) When
  the real mechanism is out of reach, refuse by name (0A000) and put it in
  the backlog; a plausible stand-in is a decision for Joe, never the
  default.
- **Always the most faithful PostgreSQL implementation** (Joe,
  2026-10-25). When there is more than one way to close a gap, build the one
  closest to what PostgreSQL itself does -- its algorithm, its data
  structures, its order of checks and its messages -- even when a cheaper
  approximation would pass today's test. Size and cost are reasons to
  sequence the work, never to settle for less. When a decision goes to Joe,
  put the most faithful option first and recommend it; his answer has been
  the faithful one every time (SSI ported from `predicate.c`, the B-tree and
  hash page models, `nodeToString` for `pg_rewrite` rather than baked
  trees).
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

  Compare tests STARTED against tests that REPORTED, and fix the hang, not
  the timeout (the Go gauge's false 100.0%: `measuring`).
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

**The Rust MongoDB server reproduces mongod's `explain`, plan cache and document order** -- read the `rust-mongodb-explain` skill before changing the router or the planner, and run its sweeps (`tools/probes/explain_commands.py`, `tools/probes/find_order.py`).

### Type-mapping strategy (the critical decision)

Documents are stored as **opaque BSON blobs**. All filtering, projection, sorting, and updates happen in Python after `bson.decode`. The storage layer never inspects document content. This is deliberate: SecantusDB's whole point is that `pymongo` cannot tell us apart from `mongod`, and any lossy intermediate representation (JSON, native column types, etc.) would break that for ObjectId / Decimal128 / int32-vs-int64 / Date-with-tz / Binary / Regex.

When secondary indexes land they will be WT indexes over typed sort-key columns derived from BSON values — not JSON, not coerced numerics.

### Engines: four separate servers

A user runs **one** server -- never a per-operator / per-`Storage` selection
inside one request path. The Rust MongoDB server is self-contained Rust; its
Python handle is only a launcher, never an operator. The Rust PG server parses
SQL with `libpg_query`, lowers it via `secantus-pgplan` to the MQL filters
`secantus-core` evaluates, and builds from **its own directory**
(`./inv rust-pgserver-build`). **Before any work on it, read the
`rust-pg-server` skill.**

#### Versioning: three independent version lines

The three lines (Python package, Rust MongoDB lockstep crates, Rust PG crates) are described in the `secantusdb-release` skill.

Bump a Rust line with `./inv rust-version-bump --to <version> [--line pg]`,
never a blanket substitution across `crates/`, which would fold the PG crates
into the MongoDB line. Release mechanics: the `secantusdb-release` skill.

**No version bumps in feature PRs.** The Rust versions are assigned at release
time, so a feature PR touches no `version` line (concurrent PRs collided on
it); describe the change in a `changelog.d/` fragment instead.

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

- **The parity suites (`tests/test_rust_*_parity.py`) no longer run in CI**
  (2026-10-20). They pinned each pure Rust engine to its Python counterpart,
  which was only ever a drift detector, and with the Python engine fully
  deprecated there is nothing to drift from. A parity failure you meet
  locally is not a bug report: the reference is `mongod`. The transitional
  in-process engine selection (`src/secantus/engine.py`) and the rest of this
  history are in the `python-servers` skill.

## Tooling

- **Ad-hoc reproducers and cross-driver smokes use `127.0.0.1:27018`** (clear of a real `mongod` on 27017; gauges use ephemeral ports).
- Python 3.12 pinned via `.python-version`. Managed with `uv`. Always invoke Python via `uv run python -m ...` so `pyenv` doesn't intercept.
- Build/admin tasks: `tasks.py` (`invoke`). `invoke test`, `invoke lint`, `invoke fmt`, `invoke docs`, `invoke serve`.
- **Admin-UI screenshots are generated, never hand-captured** (`invoke
  admin-screenshots`), and regenerated on every release. Details: the
  `secantusdb-release` skill.
- `pytest` with `pytest-xdist` parallel by default (`addopts = "-n auto"`).
  Tests must use `port=0` and a unique `storage_path` per test (`tmp_path`).
  The default suite runs real on-disk WiredTiger in **fast test-storage mode**
  (`SECANTUS_TEST_FAST_STORAGE=1`: no close-time checkpoint). That mode and
  `SECANTUS_FORCE_DURABLE=1` govern the PYTHON `Storage`; the CI lane that ran
  the suite durable was a Python-server lane and is gone (2026-10-20). The
  Rust servers' durability is covered by their own cargo and pytest tests.
  Details: `build-and-test`.
- **Never use a mock storage -- always the real WiredTiger-backed `Storage`**
  (`Storage(str(tmp_path))`, closed in the fixture teardown). The removed
  `FakeStorage` mock masked real bugs; do not reintroduce one.
- **Driver-conformance gauges** -- thirteen MongoDB-driver gauges and seven SQL
  ones run **unmodified** upstream test suites against SecantusDB (invocation,
  toolchains and quirks: `/conformance-gauges`). The weekly `validate.yml`
  runs every gauge against a Rust server, as a `*-rust-server` lane: the
  MongoDB drivers against `secantusd-rs`, the SQL gauges (`--server rust`)
  against `secantusd-pg`. **Keep `invoke validate-all
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
- **Nothing publishes to PyPI** (2026-10-20). Never run `uv publish` /
  `twine upload`, and do not push a `vX.Y.Z` tag: it no longer triggers
  anything, and the tag ruleset protects it once pushed. The PyPI project
  `SecantusDB` (trusted publisher bound to the deleted `publish.yml`) keeps
  its existing releases.
- The on-disk repo path is `/Users/jdrumgoole/GIT/SecantusDB`. The **import package** is `secantus` (`src/secantus/`, `import secantus`); the distribution name in `pyproject.toml` is `SecantusDB`.

## Releases and website

Both are skills, `secantusdb-release` and `secantusdb-website`, under
`~/.claude/skills/` (edit them there, not here). Without loading them:

- **A Rust server release ends at the live site, not at the GitHub Release.**
- **Website content lives on `main`**: edit `website/` on a short branch, PR,
  then `invoke deploy` from `main`. Its performance numbers and driver panels
  are generated -- never hand-edit them.
- **The site leads with the two Rust servers.**

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

A probe that finds something belongs in a test, or the next session re-finds
it. For the Rust servers that is a regression test beside their own tests (or
a `tests/backlog/` gap test while it is open; for the MongoDB server, a
comparison no backlog item asked for goes in `tests/mongod_live/`, which CI
runs against mongod), not `tests/test_mongod_differential.py`, which targets
the deprecated Python server.

**Parity is not correctness.** The parity suites pin the two engines to each
other, so they are equally satisfied by both being wrong (`$bucket`,
`$densify`, `$stdDev*`), and a hand-run engine-vs-engine sweep is the same
trap with nothing to warn you. When you are establishing what is correct,
leave the other SecantusDB server out of the comparison entirely.

**Before quoting any number** -- a gauge rate, a suite count, a benchmark, a
corpus divergence count -- read the `measuring` skill.

## CI is load-bearing — failures are serious bugs, not flakes

After every push (to a feature branch via PR — the default — or to `main`), check the corresponding GitHub Actions run and **resolve any failures before moving on**. CI is the source of truth. Procedure, the docs-only `paths-ignore` exception and the failure-pattern catalog live in `/ci-check` (add new patterns there). **Watch the PR's run, not `main`'s**; a `cancelled` run means you superseded it with a newer push.

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

(Measured 2026-10-15: eight agents at cargo's default jobs held the load
average at 66 on 12 cores.)

The same arithmetic is why `invoke validate-all --jobs` stays at 4 or fewer
(Tooling, above): do not run a gauge sweep beside building agents at all.

## Conventions for changes here

- **Never commit build output or any other generated binary.** No cargo
  `target*/` directory, no `.so` / `.dylib` / `.a` / `.rlib` / `.o`, no
  executable, no wheel or sdist, no `.profraw`, no core dump, no database
  store. Source and small hand-made fixtures only.
  - **Read `git status --short` before every `git add -A`**, and stage by
    path when anything in it is not a file you wrote.
  - A new build directory gets a `.gitignore` line in the same change that
    creates it.
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
  every one-file change makes the ceremony cost dwarf the work. Instead:
  - while iterating, run only the **targeted** tests (`uv run python -m pytest
    -n0 tests/test_x.py`) plus whatever proves the slice;
  - run the **full suite once** at the end, before the batch's commit;
  - one PR, one CI watch, one merge, one teardown per batch. An incidental
    discovery is folded into the batch or written down, never its own cycle.
- **Major features and non-trivial updates go in a git worktree on a feature branch — never directly on `main`.** New CRUD operators, aggregation stages, storage-layer changes, wire-protocol additions, indexing work, and similar multi-file changes all qualify. Trivial one-file tweaks (typo fixes, single-line config edits) can stay on `main`. Create a worktree alongside the repo: `git worktree add ../SecantusDB-<branch> -b <branch>`. Develop and run the full test suite (and `./inv rust-gate` for Rust-server work) there.
- **Pin a worktree to a commit for any timing / performance measurement**
  (`git worktree add --detach ../SecantusDB-measure "$(git rev-parse
  origin/main)"`): parallel sessions move `main` mid-run. The `measuring`
  skill has the rest of what makes a number trustworthy.
- **Land via branch push + PR, not `git push HEAD:main`.** Push the feature branch (`git push -u origin <branch>`) and open a PR (`gh pr create --base main`); let CI run on the PR, then merge when green. **Never push straight to `main`**: CI concurrency is per-ref, so sessions sharing `main` cancel each other's runs.
- **Cleaning up is part of finishing the work.** Tear down the branch and
  worktree in the SAME session that merges (worktree before branch; `-D` only
  after `gh pr view <N> --json state` says `MERGED`), and never remove a
  worktree, branch or stash you did not create in this conversation. The
  sequence and its traps: `batch-worktree`. Everything else a session leaves
  running: `session-cleanup`; closing a whole session: `close-session`;
  opening one: `start-session`.
- **Record the changelog with a fragment, not by editing `docs/changelog.md`.** A feature PR adds one file `changelog.d/<short-slug>.md` holding a single entry (a `###` headline + prose lede + `#### Added` / `#### Fixed` / … sections, no version number or `##` header) — see `changelog.d/README.md`. **Never edit `docs/changelog.md` or bump a version in a feature PR**; `invoke changelog-collate` folds fragments in at release.
- New CRUD operators or aggregation stages should land with both a unit test (in `tests/test_query.py` / `tests/test_update.py` / `tests/test_aggregate.py` / `tests/test_expressions.py`) and a `pymongo`-driven integration test in `tests/test_crud.py`. The integration test is the conformance proof; the unit test pins the semantics.
- Layer boundaries to defend: the wire layer never knows about commands; the command layer never knows about SQL; pure operator engines (`query`, `update`, `expressions`, `aggregate`, `projection`) take only docs in and out, no I/O.
- Errors raised inside command handlers are caught by `dispatch` and turned into `{ok: 0, errmsg, code, codeName}`. Don't leak Python tracebacks to the wire.
