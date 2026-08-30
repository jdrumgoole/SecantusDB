# Would a Rust test harness be faster? — investigation + plan

> **STATUS (2026-08-29).** The Rust-harness idea is **rejected on measurement**
> (§1–§5) and needs no further work. **L1 — the WiredTiger-home clone — is now
> IMPLEMENTED** on branch `testperf-wt-clone`: `tests/wt_template.py` +
> the `wt_home` fixture, 22 test files converted, **measured 259.9 s → 194.9 s
> (1.33x) across those files' 789 tests**, full suite green. See §11.
> **Still open:** L2 (lazy eager tables) and L3 (module-scoped fixtures).
> Backlog entry: `tasks/backlog.md` §3.6.

**Verdict: no. Don't do it.** The Python harness owns ~3 % of per-test wall-clock;
83 % is inside WiredTiger's C library and the rest is the pymongo driver, which
cannot be replaced without destroying the project's central design constraint.
The real lever is **not creating a WiredTiger database per test** — a
language-agnostic fix worth ~1.9× on the fixture floor, measured and proven
feasible below.

All numbers taken this session on a quiet 8-core Apple Silicon box at
`5372fdfa`, medians of 12 reps unless stated. Contaminated runs are flagged and
excluded — see "Measurement hygiene" at the end.

---

## 1. The premise fails at the first measurement: the suite is not CPU-bound

| run | tests | wall | user+sys CPU | cores busy | % of 8-core capacity |
|---|---:|---:|---:|---:|---:|
| full suite, `-n auto` (8 workers) | 5070 | **712.4 s** | 242.9 + 192.7 = **435.6 s** | 0.61 | **7.7 %** |
| `tests/test_crud.py`, `-n0` (1 worker) | 320 | **90.0 s** | 13.2 + 10.7 = **23.8 s** | 0.26 | **26 % of one core** |

The suite spends **92 % of its available CPU capacity idle**, waiting on disk and
sockets. Rust makes CPU work faster. There is no CPU work to speed up.

This independently reproduces the existing `tasks/test-performance-plan.md` §I4
finding ("the suite is I/O-bound, not CPU-bound"; 3× the workers bought 1.38×)
by a different route — that one inferred it from an Amdahl fit, this one measures
the idle directly.

## 2. Per-test floor: who owns the 281 ms?

`scratchpad/harness_floor.py`, each layer built on the one above:

| layer | cumulative | delta | owner |
|---|---:|---:|---|
| raw `wiredtiger_open` + `close` | 97.7 ms | 97.7 ms | **WiredTiger (C)** |
| `Storage()` open + close | 234.4 ms | +136.7 ms | **WiredTiger (C)** — 12 eager table creates |
| `SecantusDBServer` start + stop | 244.0 ms | +9.6 ms | Python (thread + socket) |
| + pymongo connect + insert | 281.1 ms | +37.1 ms | pymongo + TCP handshake |

**The +136.7 ms is WiredTiger, not Python.** Verified independently
(`scratchpad/wt_tables.py`): a bare WT connection creating N tables costs
**9.7 ms per table**, and 12 tables measured 119.0 ms — matching the delta. So
`Storage.__init__`'s own Python is ≈ 0 ms. (The 16 documents shards and 16 oplog
shards are already lazily created — PR #680 — so 12 is what remains eager.)

Attribution of the 281 ms per test:

- **234 ms (83 %) — inside WiredTiger's C library.** Identical under a Rust harness.
- **~37 ms (13 %) — pymongo.** Cannot be replaced: *"`pymongo` is the conformance
  target. Behaviour is 'correct' when a `pymongo` client cannot tell SecantusDB
  apart from a real `mongod`"* (CLAUDE.md). Swapping in a Rust driver would not
  speed the harness up — it would stop the harness from testing the thing it exists
  to test.
- **~10 ms (3.4 %) — Python the harness actually owns.** This is the entire
  addressable surface of a Rust rewrite.

## 3. The tests a Rust rewrite *would* speed up are already free

`tests/test_query.py`: **82 pure-operator tests in 0.16 s** (~2 ms/test). The pure
engines (`query`/`update`/`expressions`/`projection`/`sortkey`/`diff`) take docs in
and out with no I/O — the one place Rust wins on raw speed — and they are already
negligible against a 712 s suite.

Moreover this work is **already done**: the Rust operator engines exist
(`_secantus_core`), and **610 `#[test]` functions** already run natively under
`cargo test` / `invoke rust-test`. The Rust-side testing story is not missing.

## 4. Harness orchestration is subprocess-wait, not compute

- **pytest collection**: 4.47 s for 5069 tests, once per worker.
- **`invoke` startup**: 0.17 s user.
- **The 8 385 LOC across `*_validation/`**: `subprocess.Popen` a driver test
  process, then parse NDJSON/JUnit into markdown. Pure wait + trivial parsing.
- **`validate-all`'s ~20 min**: dominated by *toolchain builds* — mongo-c and
  mongo-cxx compiled from source, PHP ext build, JVM/Gradle, dotnet + gpg
  (`test-performance-plan.md` §2). Rewriting the Python that waits on those
  builds changes nothing about the builds.

## 5. The ceiling, stated plainly

Best case for a **total** rewrite — assume every line of harness Python vanishes
and costs zero:

| saving | amount | share of 712 s suite |
|---|---:|---:|
| per-test Python (10 ms × ~2 500 server-fixture tests ÷ 8 workers) | ~3 s | 0.4 % |
| pytest collection + interpreter startup, all workers | ~10 s | 1.4 % |
| **optimistic total** | **~13 s** | **~2 %** |

Against a cost of rewriting **80 388 LOC of tests**, abandoning pymongo as the
conformance target, and forfeiting pytest/xdist/fixtures. This is not a close call.

---

## 6. What the actual lever is (and it isn't a language)

The 234 ms of per-test WiredTiger work. Three initiatives, in payoff order.

### L1 — Clone a prebuilt WT home instead of creating one. ✅ measured, feasible

`scratchpad/wt_clone.py` builds one pristine `Storage()` home, then per "test"
copies it and opens the copy:

| approach | median | vs fresh |
|---|---:|---:|
| fresh `Storage()` | 239.8 ms | 1.0× |
| `cp -c` (APFS clonefile) + open | **126.7 ms** | **1.9×** |
| `cp -R` (plain copy) + open | 133.8 ms | 1.8× |

**WiredTiger opens the cloned home cleanly** — verified by running
`list_collections` against it. This removes the entire table-create cost
(~113 ms/test); the residual 127 ms is the unavoidable connection open.

Notably `cp -R` is within 7 ms of the APFS clonefile path, so **this is not
macOS-specific** — it should carry to Linux CI without needing reflink support.

Open questions to settle in the spike (§7 Phase 1): template lifetime
(session- vs worker-scoped), interaction with `durable=True` fixtures (the
journal is copied too), and whether the cloned home's persisted RecordId /
oplog-meta state is correct for a fresh test (it should be — every clone starts
from the same empty baseline).

### L2 — Make more of the 12 eager tables lazy

Documents and oplog shards are already lazy. `users`, `roles`,
`profile_settings`, `preimages` are only needed on demand. **Each table removed
from the open path is 9.7 ms off every test** — and unlike L1 this also benefits
**both shipped servers' startup**, not just tests. Compounds with L1 (a smaller
template clones faster).

### L3 — Module-scoped server fixtures (= I2b, already in the plan)

**70 of 267 test files** build a function-scoped `SecantusDBServer(tmp_path)`.
`tests/test_crud.py` is the type case: 320 tests, 90 s, no test slower than
0.6 s — the entire file is fixture floor. Amortising one server across a module
removes most of the 281 ms for converted files. Already scoped in
`test-performance-plan.md` §I2b; this investigation raises its priority, because
it and L1 are the only two things that touch the 83 %.

### L4 — The residual ~98 ms raw open/close

The hard floor. Only a pooled/shared WT connection beats it, which trades away
per-test isolation. Not recommended until L1–L3 are done.

---

## 7. Recommended plan

**Phase 0 — measure (done; this document).**

**Phase 1 — L1 spike.** Worktree `testperf-wt-clone`. Build the template once per
xdist worker; add a `cloned_storage` fixture; convert `tests/test_crud.py` alone
and diff wall-clock + pass-count against baseline. Adopt only on zero new
failures. *Expected: 90 s → ~45 s on that file.*

**Phase 2 — L2 lazy tables.** Per-table audit of the open path; land in the
Python `Storage` and mirror in `secantus-storage` (both servers must keep the
same on-disk layout — the layouts are byte-identical by design).

**Phase 3 — L3 module-scoped fixtures.** Per-file audit, pure CRUD/query/aggregate
first; keep oplog / change-stream / reopen / capped / TTL-clock files
function-scoped. Pair with `--dist=loadgroup` (already the default).

Re-run the full suite and the pymongo + driver gauges after each phase — the
gauges gate every merge.

## 8. Explicitly rejected

- **Rewriting the harness/conftest/fixtures in Rust** — addresses ~3 % of per-test
  cost; the other 97 % is C and pymongo.
- **A Rust test runner replacing pytest** — saves ~10 s of 712 s and forfeits
  xdist, fixtures, and the whole plugin ecosystem.
- **Rewriting the `*_validation/` gauge runners in Rust** — they wait on
  subprocesses and parse small reports; the ~20 min is toolchain builds.
- **Replacing pymongo with a Rust driver in the tests** — pymongo *is* the
  conformance target. This would delete the suite's reason to exist.

Rust effort stays pointed at the **server**, where the product's latency actually
lives and where the existing findings (PGO, thin-LTO, mimalloc; +10 % 1w /
+14 % 8w in #702) are already paying off.

## 9. Measurement hygiene

Per CLAUDE.md, parallel worktrees can move `main` mid-run. `git rev-parse HEAD`
was `5372fdfa` before and after every run here.

**One contaminated run is excluded**: a first full-suite run (685 s) overlapped
my own micro-benchmarks; its CPU ratio (0.61 cores) happened to match the clean
run, but its wall is not comparable. The clean 712 s run is the one quoted. The
`wt_clone` numbers were also re-taken clean — under load the same script reported
fresh `Storage()` at 1034 ms vs 240 ms quiet, a 4× distortion that would have
made the clone look *worse* than it is.

Note this box is **8-core**; `test-performance-plan.md`'s baseline was taken on a
12-core machine at a smaller test population (~3 785 vs 5 070). The two suites'
absolute walls are not comparable — only the structural ratios are.

Reproducers were local scratch (`scratchpad/` is untracked by convention), not
committed. Rebuilding them is short: **`harness_floor.py`** times four nested
layers (raw `wiredtiger_open`+close / `Storage()` / `SecantusDBServer` start+stop
/ + pymongo connect+insert) into a fresh `mkdtemp` per rep and prints the deltas;
**`wt_tables.py`** opens a bare WT connection and creates N tables (N = 0/5/12/23)
to derive the per-table cost; **`wt_clone.py`** builds one `Storage()` template,
then times `cp -c` / `cp -R` + open against creating one fresh. All take
`--reps` and use medians. The L1 result itself no longer needs them — it is
reproducible from §11's before/after by stashing the conversion.

## 10. Unresolved: an intermittent 2-test failure seen during this work

Three full-suite runs were taken at `5372fdfa`:

| run | result | wall |
|---|---|---:|
| 1 (contaminated) | 5032 passed | 685 s |
| 2 (clean, quoted above) | **2 failed**, 5030 passed | 712 s |
| 3 (clean rerun) | 5032 passed | 692 s |

Run 2's two failures **did not reproduce** in run 3. Their names were lost: the
run was piped through a `grep` that kept only the timing lines, and pytest's
`lastfailed` cache held only stale entries from earlier sessions (its two
`test_driver_panels.py::test_format_rate_*` entries name tests that no longer
exist — consistent with the deliberate removal of expected-failure exclusion from
the panels).

Per CLAUDE.md this is a **failing test, not a flake to wave through** — a
parallel-only, ~1-in-3 failure is exactly the shared-state/timing class the
project treats as a real bug. It is **not** caused by anything in this
investigation (no source was modified; only `scratchpad/` scripts were added).
Chasing it needs a full run with `-rf` and unfiltered output retained, repeated
until it reproduces. Logged here so it is not lost.


---

## 11. L1 implemented — results (2026-08-29, branch `testperf-wt-clone`)

**Shipped.** `tests/wt_template.py` provides `build_template()` /
`clone_template()`; `tests/conftest.py` adds a session-scoped `_wt_template`
(one pristine home per xdist worker, built by running the real `Storage`
constructor so it can never drift from the schema) and a function-scoped
`wt_home` that clones it. 22 test files converted from
`storage_path=str(tmp_path)` to `storage_path=wt_home`.

### Measured, same files, same 789 tests, serial (`-n0`), quiet machine

| | wall |
|---|---:|
| before (fresh home per test) | **259.9 s** |
| after (cloned home per test) | **194.9 s** |
| **saving** | **65 s — 1.33x, ~82 ms/test** |

The ~82 ms/test realised matches the ~87 ms predicted from the floor
decomposition (§2). `tests/test_crud.py` alone went **86.1 s → 58.4 s (1.48x)**,
each figure repeated twice.

Full suite: **5036 passed, 40 skipped** (5032 baseline + 4 new equivalence
tests), exit 0.

### Correctness

`tests/test_wt_template.py` pins the equivalence rather than assuming it: a
cloned home and a freshly-created one are driven through the same spread of
storage behaviour (collection registry, documents shards, `_id` index, secondary
index catalog + entries, oplog) and asserted **equal as whole result
dictionaries**, plus clone-starts-empty, clone-isolation (writing through one
clone reaches neither its siblings nor the template), and durable
close-and-reopen.

### Notes for whoever picks up L2/L3

- The clone is **copy-on-write** where the filesystem supports it (`cp -c` on
  APFS, `cp --reflink=auto` on Linux, `shutil.copytree` fallback). The template
  is 19 files / 10.6 MB, so on a reflink-capable filesystem this also *reduces*
  per-test disk versus creating a fresh 10 MB home — relevant to the CI
  disk-headroom problem in `backlog.md` §3.6. On a non-reflink filesystem it is
  a full copy, i.e. the same disk as before, not worse.
- `cp -c` (5.81 ms) vs `shutil.copytree` (6.22 ms) is a wash on time; `cp` is
  kept for the CoW disk benefit, not for speed.
- **The `wt_template` import in `conftest.py` must stay lazy (inside the
  fixtures).** `tests/test_crash_stall_watchdog.py` copies `conftest.py` *alone*
  into a tmp dir and runs a nested pytest session against it, with no
  `wt_template.py` alongside — a module-level import breaks all five of those
  tests at conftest load. This was hit and fixed; don't reintroduce it.
- `wt_home` returns a `tmp_path/wt` **subdirectory**, so tests can still use
  `tmp_path` for archives / exports without colliding with WiredTiger's files.
- Only the 22 files matching the exact `storage_path=str(tmp_path)` shape were
  converted in the first pass. **A second pass (2026-08-30) took the remaining
  headroom — see §12.**


---

## 12. L1 second pass — the remaining files (2026-08-30)

The first pass converted only the exact `storage_path=str(tmp_path)` shape. The
dominant remaining form was `storage_path=str(tmp_path / "<sub>")`, which is the
same thing with a subdirectory. **54 more files / 83 call sites converted.**

### Measured, same files, same 950 tests, serial, quiet machine

| | wall |
|---|---:|
| before | **349.5 s** |
| after | **252.6 s** |
| **saving** | **96.8 s — 1.38x, ~102 ms/test** |

Slightly better per test than the first pass's ~82 ms, because these files skew
towards one server per test rather than a shared module fixture.

Deliberately **not** converted: the 11 files whose servers do not all come from
`tmp_path` — backup / restore / PITR / mongodump / perf-regression. Those stand
up several stores with distinct roles (source, target, restored, archive output)
and a restore target in particular must often start *empty*, so a pre-populated
clone would change what the test proves.

### Two traps the mechanical conversion hit, both caught by tests

1. **Same literal path across several servers means one shared store.** The
   first rule was "more than one `storage_path` in a function → give each its
   own home", which broke the bootstrap-then-restart pattern: `test_x509_auth`
   starts a server, creates a user, stops it, then brings the real server up
   **on the same path**. Handing those two calls separate homes silently loses
   the user. The rule is now keyed on the number of *distinct* literals, not the
   count — and with that fix, no function needs more than one home, so no
   factory fixture was added.
2. **Only convert what pytest injects into.** `test_getmore_batching` has a
   plain `@contextlib.contextmanager` helper taking `tmp_path`; its callers pass
   it positionally, so renaming the parameter to `wt_home` handed it a `Path`
   where a `str` was expected and surfaced as
   `TypeError: in method 'wiredtiger_open'`. The converter now skips anything
   that is not a `test_*` function or a `@pytest.fixture`.

### Disk

Worth recording alongside the wall-clock: one full-suite run was measured
leaving **47 GB** in `$TMPDIR/pytest-of-<user>`. Every converted test now takes a
copy-on-write clone where the filesystem supports it instead of building a fresh
~10 MB store, so the converted share of that footprint drops too. That matters
beyond disk: a `pytest-of` backlog makes every *later* run pay an unbounded
`rmtree` at exit (`tasks/backlog.md` §3.6).
