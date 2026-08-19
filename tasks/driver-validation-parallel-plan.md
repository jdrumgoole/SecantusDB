# Driver-validation parallelization plan

> **DELIVERED (audited 2026-08-20).** The parallel gauge runner exists:
> `invoke validate-all --jobs` (tasks.py:1545, default 4) fans the thirteen driver
> gauges out concurrently, `validate-all-servers` (1649) does both servers, and the
> per-gauge tasks take `--jobs` too. The concurrency ceiling this plan worried about
> is now a documented rule in CLAUDE.md — keep `--jobs` at 4 or fewer, because
> beyond that the daemons contend and timing-sensitive driver tests flake.

Goal: cut wall-clock on the five driver-conformance gauges (`pymongo`,
`go`, `node`, `java`, `ruby`) without compromising the property that
each gauge runs unmodified upstream driver tests against a real
on-disk SecantusDB server.

## Current state (the bottleneck)

| Gauge   | Driver tests                                                            | SecantusDB instance              | Dominates wall-clock?        |
| ------- | ----------------------------------------------------------------------- | -------------------------------- | ---------------------------- |
| pymongo | `-p no:xdist` (forced serial — pymongo tests share DBs)                 | embedded, in-process, `:memory:` | **yes — slowest**            |
| go      | `go test` (per-test `t.Parallel()` available, unconfigured)             | subprocess on `:27018`, `:memory:` | medium                       |
| node    | mocha serial by default                                                 | subprocess on `:27018`, on-disk  | medium                       |
| java    | gradle `:bson:test` (no `maxParallelForks` flag)                        | subprocess on `:27018` (unit-only — bson never hits the daemon) | medium                       |
| ruby    | rspec serial                                                            | subprocess on `:27018`, on-disk  | small                        |
| **CI**  | **5 invokes in series on one ubuntu-latest runner**                     | —                                | **sum of all five (~30–40m)**|

Three independent bottlenecks:

1. CI runs the gauges serially on a single runner.
2. All gauges hard-code port `27018` so they can't co-run on one host either.
3. Within each driver, parallel execution is either disabled (pymongo)
   or unconfigured (go / node / java / ruby).

And one **policy violation** worth fixing first: two of the runners
(`pymongo_validation/plugin.py`, `go_validation/runner.py`) use
`:memory:` storage. The project policy (`CLAUDE.md` → "Tooling") is
that the default suite runs against real on-disk WiredTiger so the
schema, persistence, and close-and-reopen paths are continuously
exercised. The gauges are conformance runs; they should respect the
same policy. Only `tests/test_perf_regression.py` retains `:memory:`
(stable baselines are its whole point).

## Phase 0 — Move every gauge daemon to on-disk storage

**~½ day; required baseline before any parallelism work.**

- `pymongo_validation/plugin.py`: replace `storage_path=":memory:"`
  with a `tempfile.mkdtemp(prefix="secantus-pymongo-gauge-")` allocated
  in `pytest_configure`, `shutil.rmtree` in `pytest_unconfigure`.
- `go_validation/runner.py`: drop `--storage-path :memory:` in favour
  of a tempdir, cleaned in the `finally` block (mirroring node / ruby).
- Leave node / ruby alone — already on-disk because their auth-seeding
  restart needs persistence. Matches policy by accident.
- Java: no daemon spawn (bson-only), no change.

**Run this in isolation first.** Any new gauge failure surfaced by the
switch is a real persistence-path bug that `:memory:` was hiding —
diagnose and fix it before layering parallelism on top.

## Phase 1 — CI matrix across drivers

**~5× CI wall-clock improvement; 1 day; low risk.**

Split `.github/workflows/validate.yml`'s single `validate` job into a
matrix:

```yaml
strategy:
  fail-fast: false
  matrix:
    driver: [pymongo, go, node, java, ruby]
```

Each matrix entry runs only its own toolchain setup + only its own
`invoke validate-<driver>`. The PR-opening step becomes a follow-on
`needs: validate` job that downloads each driver's report artifact and
runs the diff. Today's serial ~30–40 min run drops to roughly the
slowest gauge (pymongo, ~8–10 min).

Cost: 5 GitHub-runner-minutes vs 1, but only weekly + manual-dispatch —
negligible.

Each matrix job is its own GitHub runner with its own filesystem, so
the on-disk-daemon constraint from Phase 0 doesn't introduce any
cross-job contention.

## Phase 2 — Port-0 + tempdir per daemon spawn

**1 day; low risk; unlocks Phase 3 and 4.**

Replace `DAEMON_PORT = 27018` in `go_validation/runner.py`,
`node_validation/runner.py`, `ruby_validation/runner.py`,
`java_validation/runner.py` with:

1. Bind a kernel-assigned ephemeral port at runner startup
   (`socket.bind(("127.0.0.1", 0))` → read port → close).
2. Pass `--port <picked>` to the daemon.
3. Build `MONGODB_URI` / `org.mongodb.test.uri` from the picked port.

Every daemon spawn pairs that ephemeral port **with a fresh
`tempfile.mkdtemp()`**. No two daemons ever share a storage dir, even
when run in series within a runner (avoids leftover lock files from a
previous abort).

`CLAUDE.md` → "Tooling" says `27018` is the canonical port "for
everything else"; that's still right for ad-hoc reproducers, but the
gauges aren't constrained to it — they print the picked URI to stderr
already, so debuggability is preserved. Update the CLAUDE.md "Tooling"
entry to note "gauges use ephemeral ports; ad-hoc tools still use
27018."

Add `invoke validate-all` that fans out the 5 invokes via
`concurrent.futures.ThreadPoolExecutor(max_workers=5)` and collates
exit codes. Locally that's ~5× parallel.

## Phase 3 — In-driver parallelism, one on-disk daemon per worker

**2–3 days; medium risk; ~2–4× speedup within each gauge.**

The driver test suites all support some flavour of parallel execution;
the missing piece is per-worker daemon isolation. Pattern: **each
worker gets its own SecantusDB on its own ephemeral port with its own
on-disk `tempfile.mkdtemp()`**.

The on-disk constraint shapes the design here:

- Each parallel worker spawns its own daemon with its own
  `tempfile.mkdtemp()`. A worker pool of N means N concurrent WT
  instances on disk.
- WT startup cost on-disk is ~50–100ms vs ~5ms for `:memory:`. With N
  workers the cost is N× a tempdir + WT init at gauge start, paid once
  per gauge run; in test-loop terms it's negligible. Cleanup is N×
  `shutil.rmtree` at gauge teardown.
- **File-descriptor budget.** Each WT instance opens ~10–20 FDs (data
  files + journal + lock). At N=8 workers × 5 driver gauges run in
  parallel locally (worst case via Phase 2's `validate-all` fanout),
  peak is ~600–800 FDs. macOS default soft limit is 256 —
  `validate-all` should bump it via
  `resource.setrlimit(RLIMIT_NOFILE, ...)` at startup, with a fallback
  warning if the hard limit is too low.
- **Disk pressure.** Each on-disk WT instance writes a journal as
  tests run. For pymongo's ~600 tests, expect 50–200 MB scratch per
  worker. With 8 workers that's ~1–2 GB transient — fits comfortably
  on any dev laptop but cap `validate-all`'s `max_workers` at
  `min(os.cpu_count(), 8)` so we don't fan out to 32 on a Mac Studio.
- **Tempdir base.** Default `tempfile.gettempdir()` is `/tmp` on Linux
  (often tmpfs — defeats the "on-disk" intent) and `/var/folders/.../T`
  on macOS (real disk). Honour `$SECANTUS_GAUGE_TMPDIR` if set so CI
  can pin tempdirs to a real-disk path on Linux runners.

Per-driver:

- **go**: `go test -parallel <N>` already exists. Bottleneck is the
  shared daemon — concurrent `t.Parallel()` tests collide on DB names.
  Two options:
  1. Keep one daemon but ensure tests namespace their DBs by
     `t.Name()` (most already do — measure first).
  2. Fan out N daemons + write a URI per package. `vendor/mongo-go-driver`
     is unmodified, so the shim lives in `go_validation/` and is
     exposed as a `MONGODB_URI_TEMPLATE` env var.

  Start with (1) and measure.
- **node**: `npx mocha --parallel --jobs <N>` — needs each worker to
  pick its own URI. Wrap mocha invocation so each worker spawns its
  own daemon (mocha has a `parallel` option with `--require <file>`
  hook; that hook can start the daemon + tempdir and inject
  `process.env.MONGODB_URI` inside the worker).
- **java**: `:bson:test` is pure unit; doesn't touch the daemon. Add
  `-PtestMaxParallelForks=<N>` (gradle wrapper already respects `-P`)
  — no daemon-per-worker needed for this module.
- **ruby**: `parallel_rspec` from the `parallel_tests` gem. Vendored
  `Gemfile.lock` would need an entry — that **violates the "unmodified
  vendored tree" promise**. Skip unless we add `parallel_tests` to a
  shimmed `Gemfile.local`. Lowest priority: ruby is the smallest gauge.
- **pymongo**: still serial (this is the floor). The pymongo tests
  share DBs by design and we explicitly opt out of xdist for that
  reason. Phase 4 addresses this differently.

## Phase 4 — Pymongo shard-by-file (only if pymongo is still the floor)

**2 days; medium risk; ~3–4× pymongo speedup.**

Pymongo tests within a file share fixtures and DB names, but **across
files** they're isolated. Shard
`pymongo_validation/include_paths.INCLUDE` into N groups by file
(round-robin or balanced by historical runtime), launch N pytest
processes each with their own `pytest_configure` embedded server.
Each shard gets its own `tempfile.mkdtemp()` (from Phase 0) and its
own raw-json output; `generate_report.py` accepts a list of shards
(or we extend it minimally).

Worth doing only after Phase 3 lands and pymongo is still gating
wall-clock.

## Phase 5 — Cleanup wins (small but cheap)

- Drop the 5s SIGTERM grace in each runner's `finally` block to 1s —
  saves 4s × 5 runners = 20s per gauge run. Daemons close cleanly on
  SIGTERM; the long grace exists for "what if it hangs?" which becomes
  a SIGKILL anyway.
- Cache `~/.gradle/caches/`, `~/go/pkg/mod`,
  `vendor/mongo-ruby-driver/.bundle`,
  `vendor/node-mongodb-native/node_modules/` in the CI workflow via
  `actions/cache@v4` keyed on the submodule SHA. Cold-CI runs drop by
  minutes; warm runs unchanged.
- Replace the 100ms `_wait_for_listener` polling loop with
  `socket.setblocking(False)` + `select()` — saves up to 100ms per
  gauge startup. Marginal but trivially correct.
- **Stale-tempdir sweeper in `invoke clean`.**
  `glob.glob('/var/folders/**/secantus-*-gauge-*')` (and `/tmp/...`
  on Linux) + `shutil.rmtree` for anything older than 1h. Aborted
  gauges currently leak tempdirs; this is the cure.

## Suggested order

1. **Phase 0** (on-disk everywhere) — fix the policy violation before
   anything else; surfaces any latent persistence bugs in isolation.
2. **Phase 1** (CI matrix) — immediate ~5× CI win, no code changes
   outside `.github/workflows/`.
3. **Phase 2** (port-0 + tempdir per spawn) — unblocks everything else.
4. **Phase 5** (cleanup + caches) — cheap, lands alongside Phase 2.
5. **Phase 3** (in-driver parallel) — biggest absolute gain on dev
   laptops.
6. **Phase 4** (pymongo shard) — only if measurements still show
   pymongo as the floor after Phase 3.

## Trade-offs to flag

- **The headline cost of moving everywhere to on-disk is that the
  pymongo gauge will get slower** — currently `:memory:`, the switch
  to a tempdir adds (rough estimate) 20–40% to its wall-clock from WT
  journal-write latency. That's the price of actually exercising the
  persistence path during conformance runs. Phases 1, 3, and 4 absorb
  most of that increase.
- Phase 0 may expose real bugs that `:memory:` was masking — treat any
  new gauge regression as a real-MongoDB-parity bug, not as
  "Phase 0 broke things", and fix it before continuing.
- The **"unmodified vendored driver tree" property is load-bearing**:
  it's the reason real-mongod parity is meaningful. Phase 3 must
  inject configuration via env vars / system properties / mocha hooks
  **in our runner code**, never via edits to `vendor/`. Ruby's
  `parallel_tests` is the one corner that doesn't fit cleanly — leave
  ruby serial rather than touch its Gemfile.
- Phase 4 (pymongo sharding) changes test grouping but each shard
  still sees an unmodified pymongo source tree. Safe.
- More parallel daemons → more WT instances on disk. 5–10 on a dev
  laptop is fine; 50+ might exhaust FD limits. Cap worker counts to
  `min(os.cpu_count(), 8)` and bump RLIMIT_NOFILE preemptively.
- `:memory:` retains exactly one home:
  `tests/test_perf_regression.py`, where stable in-memory baselines
  are the whole point of the gauge.
