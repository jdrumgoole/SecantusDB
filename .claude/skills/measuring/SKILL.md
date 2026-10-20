---
name: measuring
description: Before believing, quoting or publishing ANY number from SecantusDB -- a gauge pass rate (pymongo, driver gauges, psycopg), a test-suite count, a benchmark or latency table, a corpus divergence count, a timing or scaling curve. Fires on "what's the pass rate", "run the gauge", "benchmark", "how fast", "measure", "timing", "validate-all", "is this number right", before editing a number on the website, in docs/benchmark.md or in a validation report, and when a run finished suspiciously fast or suspiciously clean. Holds the provenance checks that catch a stale binary or venv, the wall-clock tell, the started-versus-reported check that catches a truncated run, the pinned worktree for timing, and how to run gauges without flaking them or flooding the session.
---

# A number is evidence only when you know what produced it

Every wrong number this project has published came from a measurement that
ran, finished and printed something plausible. The checks below are cheap;
skipping them has cost a wrong figure on the live website more than once.

## 1. Know which build you measured

- **A stale artifact is the most common lie.** The first Rust PG gauge figure
  published (73.8%) was taken by a sub-agent against a checkout 116 commits
  behind `origin/main`, 50 of them in the crate it measured. It went on the
  live site before anyone ran `git log`.
- **Before quoting:** `git rev-parse HEAD`, then
  `git log HEAD..origin/main -- <the crate you measured>`. A binary prints
  the `crates/` tree it was built from: `secantusd-rs --version`,
  `secantusd-pg --version`; compare it with `git rev-parse HEAD:crates`.
- **`tools/provenance.py` checks this for you on most paths**: the pytest
  collection hook, both probe launchers, `gauge_common.rust_binary()`, the
  psycopg gauge runner and the four `bench/` harnesses that feed published
  tables ABORT on a stale artifact. `SECANTUS_ALLOW_STALE_ARTIFACT=1`
  overrides it (and still prints) for deliberately measuring an OLD build, or
  when the stale thing is genuinely not under test -- say which in the
  report.
- **Still on you:** anything launched by hand, a new harness that does not
  call `provenance.py`, and an artifact with no stamp at all (the check
  abstains rather than guesses, so silence is not proof of freshness). A
  rebase or pull during a build leaves stale binaries behind too.
- **The venv is an artifact as well.** `uv sync` does not rebuild the
  `secantus-core` path dependency, and a venv without `_secantus_core`
  uncollects ~1,700 parity tests and still exits 0. `./inv sync`, and the
  `batch-worktree` skill for a worktree.

## 2. The wall-clock tell

**A run that finishes far faster than usual stopped reaching the code.** The
stale PG gauge run took 82 s; the honest one 1,041 s. Treat an implausibly
fast run as stale until proven otherwise. This works for every kind of
number, including a stale artifact that runs to completion and merely answers
differently. Know the usual duration of what you run (psycopg gauge ~17-20
min debug, full pytest suite ~12-13 min) and compare.

## 3. Count what started against what reported

**A test that does not complete is a failure, never a pass, and a truncated
run looks BETTER the more tests go missing.** The Go gauge published 100.0%
for months over a run a hung DNS test killed at 476 of 481: `go test`
panicking on `-timeout` emits no event for tests still in flight, so the
summariser divided what it could see. Before believing any suite number:

- compare tests STARTED with tests that REPORTED a result
  (`./inv validate-psycopg-rust` and `psycopg_validation/check_run.py` do
  this for the psycopg gauge; others may not);
- compare the collected count with the last known-good run;
- fix a hang at its cause; raising a timeout or suppressing the rate is not a
  fix (both were tried).

A chained command reports the LAST command's exit: `cmd > log; tail log`
says success whatever `cmd` did. Check `$?` on its own, or read the summary
line.

## 4. Timing needs a frozen tree and a quiet machine

- **Pin a worktree to a SHA for any timing, baseline or scaling curve.**
  Parallel sessions move `main` mid-run; one scaling curve was invalidated
  when `main` moved under it and the suite grew ~1,400 tests between runs.
  `git worktree add --detach ../SecantusDB-measure "$(git rev-parse
  origin/main)"`, run every comparison there, and confirm `git rev-parse
  HEAD` is unchanged before and after.
- A fresh worktree has no `vendor/wiredtiger`, and the copied-`.so` venv
  trick can throw WiredTiger `Session__freecb` errors on `close()` that
  inflate runtime timings; for close-to-metal timing measure in a built
  venv or build WiredTiger in the worktree (`tasks/test-performance-plan.md`).
- **Check `uptime` first.** The machine has 12 cores and is shared by two
  sessions; leaked shells and orphaned xdist workers have held the load
  average at 40 on their own. A load well above 12 makes any benchmark and
  any timing-sensitive test meaningless. Do not run a gauge sweep beside
  building agents.
- Debug builds are not release builds: say which you timed. The PG server's
  release build is fat-LTO plus PGO; a debug psycopg run takes ~3-4x longer.

## 5. Running gauges

- **Run full gauges in a sub-agent**, not the main session: they stream
  thousands of lines. Have it report the pass/fail/skip counts, the rate,
  the verbatim `FAILED ...` ids, and the started-versus-reported check.
  Iterate on one test in the main session with `invoke validate-one <nodeid>
  --server <python|rust>`.
- **Name the server explicitly** (`./inv validate --server rust`).
  The task sets `SECANTUS_GAUGE_SERVER` from its own `--server` flag
  (default `python`), so an exported `SECANTUS_GAUGE_SERVER=rust` is
  overwritten and the run silently measures the Python server. Rebuild first:
  `./inv rust-server-build` (MongoDB) / `./inv rust-pgserver-build` (PG).
- **`invoke validate-all --jobs` 4 or fewer; 1 for a committable number.**
  Beyond ~4 concurrent gauges, CPU contention pushes `hello` / `getMore`
  past drivers' `serverSelectionTimeoutMS` (Go's `TestIndexView` and
  pymongo's `test_aggregate_cursor_blocks` go first): `--jobs 8` gave
  pymongo 1019/9 against the serial 1020/8.
- Per-gauge invocation and quirks: the `conformance-gauges` skill.

## 6. Publishing a number

- **Re-derive, never inherit.** A number copied from a doc, the backlog, a
  previous session or this skill is a claim; re-run it, and cite the build,
  the date and the reference version (`mongod --version` down to the patch;
  a patch bump has changed an error message here).
- Compare like with like: two gauges cover different test populations unless
  you checked (the Python and Rust psycopg runs did not), and a percentage
  against the other SecantusDB server is not a correctness figure.
- **Published surfaces are generated; regenerate, do not hand-edit.**
  `docs/benchmark.md` (gated by `tests/test_benchmark_table_fresh.py`), the
  concurrency chart blocks (`tests/test_concurrency_chart.py`), and the
  website driver panels (`validation_summary/driver_panels.py`, which
  refuses empty panels, truncated runs and stale artifacts unless
  `--allow-stale`). Performance prose around those tables goes stale
  silently: three published claims were wrong in 2026-09, one contradicting
  the table under it. `invoke release-benchmark` re-measures against mongod
  at release time.
