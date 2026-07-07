# Test-cycle performance plan

Goal: cut wall-clock across the three test cycles without losing coverage or
durability fidelity. Every number below was measured this session on a 12-core
Apple-Silicon Mac; estimates are flagged `(est)`.

## Guardrails (non-negotiable)

- **No coverage laundering.** Never deselect/skip/xfail a *storage or CRUD*
  test to go faster. Out-of-scope tests (replica-set routing, sharding) may be
  deselected with a documented reason; correctness tests may not.
- **On-disk WiredTiger stays continuously exercised.** Schema creation, the
  B-tree, and close-and-reopen must keep running on real disk. Speed-ups may
  drop the *journal* (durability the tests throw away) but not the on-disk
  engine itself.
- **Flaky == failing.** Any parallelisation change that introduces an
  intermittent failure is a regression, not a tuning artifact.
- **Gauges must not regress.** The pymongo + driver conformance numbers are the
  product's headline; every change re-runs them.

---

## 1. Measured baseline

| cycle | command | wall | shape |
|---|---|---:|---|
| Inner-loop unit suite | `invoke test` (`-n auto`, 12 workers) | **~245–260 s** | ~3785 passed, 15 skipped |
| pymongo gauge (sync) | `invoke validate` (`-n1`) | **134 s** | 1226 passed / 5 failed (post read-pref fix) |
| pymongo gauge (async) | `invoke validate-pymongo-async` (`-n1`) | ~135 s (est) | same fix applied |
| Full driver matrix | `invoke validate-all` | **~20 min** | 13 gauges, mostly serial |

> **Baseline caveat.** Measured across a session in which `main` advanced under
> the runs (`3a86d9e5` → `b6b20df7`, a parallel worktree), growing the suite from
> 2378 to ~3785 tests. The 229 s / 2378-test figure was the earlier tip; the
> current tip is ~3785 tests and the last clean full-population run was **245 s at
> -n12 (minus one stress test)**. **Re-take a clean `invoke test` baseline on a
> quiet tree** before treating any absolute number as authoritative. The
> *structural* measurements below (fixture floor, gauge decomposition,
> validate-all split) are count-independent and stand.

Harness-floor micro-benchmark (per-test fixture cost, from `floor.py`):

| component | on-disk WT | `:memory:` |
|---|---:|---:|
| server start/stop | **266 ms** | 4.4 ms |
| client connect + 1 op | 2.6 ms | 2.6 ms |
| full fixture / test | **297 ms** | 13 ms |
| projected floor, 2378 tests @ -n12 | **~59 s (23%)** | ~2.6 s |

The 266 ms is entirely WiredTiger's on-disk durability: journal
(`log=(enabled=true)`) preallocation on open, 11 table-file creates, and a
checkpoint fsync on `close()`. `:memory:` skips all three → 60× faster.

---

## 2. Where the time goes

### Inner-loop unit suite (229 s)
1. **Per-test WT server start/stop — ~59 s (23%).** ~30 test files use a
   function-scoped `server(tmp_path)` → a fresh WT connection per test.
2. **One stress test — 43.5 s.** `test_rapid_teardown_under_read_load_drains_cleanly`
   is unmarked (runs every inner loop) and can't parallelise against itself →
   ~19% of the critical path.
3. **PITR / backup / archive tail — ~120 s CPU across ~16 tests (4–13 s each).**
   Datasets are tiny (`range(20)`); the cost is checkpoint + tar + untar +
   reopen — inherent to what they verify.
4. **The other ~2340 tests** — thin, ~0.05–0.1 s each.

### pymongo gauges (134 s, serial)
- Embedded server started **once** (floor = 0.27 s, irrelevant).
- `-n1` because upstream tests share DB names.
- Change-stream `awaitData`/tailable blocking band: ~20 s (~1 s × ~20 tests) —
  in-scope, inherent.
- `test_numerous_inserts` 13 s + 6.5 s — real work.
- (The 90 s secondary-read-pref timeout band was removed this session.)

### validate-all (~20 min)
- **Test execution across all 13 gauges sums to < 10 min.** The rest is
  **build + toolchain**: mongo-c and mongo-cxx compiled from source (CMake),
  PHP ext build, JVM/Gradle for Java+Kotlin, dotnet build + gpg libmongocrypt
  verify, plus serial gating (C++ binds 27017).
- PHP-ext: 147 s dominated by per-`.phpt` process spawn (712 processes).

---

## 3. Initiatives (prioritised)

### Inner-loop suite

**I1 — Mark the 43.5 s stress test (and peers) `slow`.**  *[cheap / high]* — ✅ **DONE (branch `testperf-phase1`)**
`test_rapid_teardown_under_read_load_drains_cleanly` now carries
`@pytest.mark.slow` (excluded from the default suite; passes under `-m slow`).
Added a `Slow tests` CI step (`.github/workflows/test.yml`, ubuntu-only) running
`-m slow` — which also restores coverage of a pre-existing slow test in
`test_concurrency.py` that had been running **nowhere** (no `-m slow` lane
existed). Iteration count deliberately left at 12 — intermittent-race coverage.
`slow` is already excluded from the default `addopts`; the stress test just
isn't tagged. Add `@pytest.mark.slow`; run `-m slow` in a dedicated CI lane +
nightly so coverage is preserved. Audit other >5 s tests for the same.
- Impact: removes the longest serial pole from the inner loop. Exact wall drop
  pending the `n12_no_stress` measurement (§5).
- Effort: trivial. Risk: low (must wire a CI `slow` lane).

**I2a — Test-mode WT config: no journal, no close-checkpoint.**  *[medium / high]* — 🔬 **MEASURED (validated)**
Add `Storage(..., durable=False)` opening with `log=(enabled=false)` and
skipping the checkpoint on `close()`. Keeps on-disk schema / tables / B-tree /
reopen fidelity (guardrail satisfied) while cutting start/stop hard.
- **Measured** (raw-WT prototype, main venv, `scratchpad/wt_floor.py`, replicating
  the real 12-table open + close): durable open+close **~245 ms** (median;
  **spikes to >2 s under parallel disk load** — the journal + checkpoint fsync
  serialise under contention) vs nodurable **~52 ms, stable** (journal off, no
  checkpoint, all 12 tables still created on disk). **~79 % faster per instance;
  aggregate ~79 s → ~17 s** over ~3848 tests @ -n12 — and *more* than that under
  real -n12 fsync contention, since nodurable removes the fsync entirely.
- Effort: thread one flag `SecantusDBServer → Storage`; audit which fixtures
  need durability. Risk: medium — persistence / reopen / PITR fixtures MUST keep
  `durable=True` (they assert journal-replay / checkpoint recovery). Opt-in;
  default stays durable so the guardrail holds.
- The current tip already dropped journal `prealloc` (`prealloc=false`), so the
  245 ms is post that win; the remaining cost is journal writes + close fsync.

**I2b — Module-scoped server fixtures with per-test namespaces.**  *[med-high / high]*
Convert the ~30 function-scoped `server(tmp_path)` fixtures to module scope,
giving each test a unique db/collection name. Amortises the 266 ms across
20–50 tests/module. Start with pure CRUD/query/aggregate files; keep oplog /
change-stream / reopen / capped / TTL-clock files function-scoped (they need a
private server).
- Impact: near-eliminates fixture cost on converted files.
- Effort: per-file audit. Risk: medium (shared oplog/cluster-time). Pairs with
  `--dist=loadgroup` so a module's tests stay on one worker.
- Sequence **after I2a** (I2a is lower-risk and independent).

**I3 — Shrink the PITR/archive tail.**  *[medium / medium]*
Shared pre-built base-archive fixture reused across restore tests; put the
cluster in a balanced `xdist_group` so it doesn't pile on one worker; mark
redundant archive-format variants `slow`.
- Impact (est): ~20–40 s. Effort: medium. Risk: low-med (keep each scenario).

**I4 — xdist balancing.**  *[cheap / pending]*
Decide `loadgroup` vs `loadscope` and worker count from the §5 scaling curve.
If wall is flat 12→8, we're serial-tail-bound (fix via I1/I3); if it scales,
spread the heavy tests so they don't serialise on one worker.

### pymongo gauges

**G1 — Parallelise the gauge.**  *[high / high]*
Spike `--dist=loadfile -n4`: whole files per worker. Most upstream test classes
drop their own DB in `tearDown`, so file-level sharding may be parallel-safe.
**Audit for flakes vs the `-n1` baseline before adopting** (guardrail).
- Impact (est): 134 s → 40–70 s. Risk: high (shared-state flakes).
- Fallback: per-worker DB-name prefix injected by the embedded plugin.

**G2 — Keep pruning out-of-scope timeout tests.**  *[cheap / incremental]* — ✅ **AUDITED (no change needed)**
The 5 remaining pymongo-sync failures (`test_index_hashed`, `test_index_text`,
`test_maxtime_ms_message`, `test_to_list_csot_applied`, `test_where`) all fail
*fast* — none appear in the slow-duration tail, so there's no further 30 s/120 s
waste to prune there. The other 11 gauges were mined earlier and have no
secondary-read-pref / server-selection timeout band. Nothing to remove; revisit
on the next pymongo bump.

### validate-all

**V1 — Cache from-source driver builds.**  *[medium / very high]*
ccache for mongo-c / mongo-cxx; cache the built PHP `.so`; persist the CMake
build dirs; key caches on submodule SHA + compiler version. In CI use
`actions/cache`; locally stop cleaning build dirs between runs.
- Impact: potentially halves validate-all (the ~10 non-test minutes).

**V2 — Reuse toolchain daemons.** Gradle daemon shared across Java+Kotlin
(same monorepo); dotnet incremental build; skip the gpg re-verify when
libmongocrypt is unchanged.  *[medium / medium]*

**V3 — `run-tests.php -j N`** for the PHP-ext gauge's 712 process spawns; keep
`validate-all --jobs ≤ 4` (C++ serial on 27017).  *[cheap / modest]* — ⏸ **DEFERRED**
Parallel `.phpt` all hit one shared SecantusDB daemon; that's the exact
contention CLAUDE.md flags as a flake source at high concurrency. Landing it
without a local PHP toolchain to prove "zero new flakes vs serial" would risk
the flaky==failing guardrail. Revisit with the PHP gauge runnable + a
flake-diff run.

### CI (cross-cutting)

**C1 — Cache the WiredTiger wheel build** (cibuildwheel), keyed on
`vendor/wiredtiger` SHA + the `cmake/patch_wt_*.py` scripts. Biggest CI sink.
**C2 — Two lanes:** fast unit lane per-PR (excludes `slow`/gauges); heavy lane
(gauges + `slow` + cross-platform) on merge/nightly.

---

## 4. Recommended sequencing

- **Phase 1 (days, low risk):** I1, G2, V3, I4 config. Fast wins, no structural
  change.
- **Phase 2 (weeks, structural):** I2a → I2b, V1, C1/C2, V2.
- **Phase 3 (ambitious):** G1 (parallel gauge), I3 (archive fixture sharing).

Re-run all four baselines after each phase; the pymongo + driver gauges gate
every merge.

---

## 5. Measurements to take first (de-risk before building)

**Take all of these on a QUIET tree** — a first attempt at the scaling curve this
session was invalidated because `main` advanced (`3a86d9e5` → `b6b20df7`) between
runs, so the three runs saw different code, different test counts (2378 vs 3785),
and a transient `_RANGE_TAGS` breakage. Confirm `git rev-parse HEAD` is stable
before and after, or run in a dedicated worktree pinned to a commit.

- **Clean `invoke test` baseline** at -n12 on the current tip — the §1 number is
  provisional.
- **xdist scaling curve** (`-n4/-n8/-n12`) + **stress-test tail isolation**
  (now automatic — the stress test is `slow`, excluded by default) — decides
  parallelism-bound vs serial-tail-bound, which sets the I1-vs-I2 payoff.
  ⏳ **STILL PENDING** — two attempts this session failed: (1) `main` moved
  mid-run; (2) the pinned worktree's copied-WT venv threw `Session__freecb`
  close errors that inflated timing, and disk load was variable. Redo in a
  worktree with a **properly built** WT (submodule + `uv sync`) on a quiet
  machine, or accept a rebuild.
- **Prototype I2a** (`durable=False`) — ✅ **DONE** (see I2a above:
  ~245 ms → ~52 ms, validated via `scratchpad/wt_floor.py`).
- **Gauge parallel-safety spike** (`--dist=loadfile -n4`) — diff failures vs the
  `-n1` baseline; adopt only if zero new flakes.

> Lesson baked in: this repo runs parallel worktrees, so `main` can move mid-run.
> Any perf measurement must pin a commit (dedicated worktree) or it's noise.
