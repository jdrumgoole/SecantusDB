# Remaining work — a plan built on measurement, not on the backlog text

> **Written 2026-08-27. State marked 2026-08-28.** Read the "How this ages"
> section at the bottom before trusting anything below. The dated *evidence*
> here is durable; the *priorities* are a snapshot.
>
> **Where we are:** Phase 0 **complete** (both halves). Phase 1 **~2 of 10 items**
> substantially done. Phase 2 **not started**. 17 bugs fixed across the sweep,
> six of them crash-class or silent wrong data.
>
> Phase 0 was scoped as bookkeeping and produced **six new bugs** alongside the
> six stale claims it retired — so "verification" has been worth more than its
> billing, and the per-item scoping in Phase 1 should still be treated as
> provisional until each item is reproduced.

## Start here on a fresh session

Everything below assumes you have reproduced the item you are about to work.
These are the things that cost time to rediscover.

**Oracles — both are the point of this plan.**

* `mongod` is on `PATH` (Homebrew `@6.0` symlink, reports 6.0.16). Start one on
  `127.0.0.1:27019` and diff against it. `tests/test_mongod_differential.py`
  (57 cases) is the standing harness; run it with `-m differential`.
* **A live PostgreSQL 14 runs on this box** at `host=127.0.0.1 port=5432
  dbname=postgres user=jdrumgoole`. `SECANTUS_PG_ORACLE_DSN` points
  `test_subms_predicates_match_real_postgres` at it. It settled six SQL claims
  in an afternoon; use it for every SQL question.

**Provision a worktree venv COMPLETELY, including `_secantus_core`.** Its
absence is silent: the 1600+ engine-parity tests do not collect and the suite
still exits 0. That was caught only by comparing the pass count to a known
baseline (6208 vs 7932). Recipe: `uv venv --python 3.12 .venv-test`, copy
`wiredtiger/` from the main `.venv`, install the dev deps **plus** `anyio
fastapi starlette trustme cryptography httpx pg8000 sqlalchemy
sqlglot==30.12.0 psycopg[binary]==3.3.4`, then
`uvx maturin build --release --manifest-path crates/secantus-core-py/Cargo.toml`
and `uv pip install --reinstall` the wheel.

**Gates that are easy to get wrong.**

* `cargo clippy` is **not** `cargo test`. Both are needed after a Rust edit —
  skipping the latter cost a red CI lane.
* The WT-linked crates (`secantus-storage`, `-storage-adapter`, `-wt`,
  `secantusdb`) are **excluded from the clean workspace**; gate them from their
  own directories with `SECANTUS_WT_INCLUDE` / `SECANTUS_WT_LIB` pointed at a
  built WT (`<repo>/build/*/wt-build`) and `LIBCLANG_PATH` at Xcode's.
* Full suite: `PYTHONPATH=src .venv-test/bin/python -m pytest tests/ -n auto -q
  --ignore=tests/test_perf_regression.py`. Expect ~8100 passed.
* Never `pkill -f` by process name while a suite runs — it kills the suite's own
  servers and invalidates the run.

**Two failure patterns that recur, both greppable and both worth a sweep.**

1. *A comment justifying behaviour by what the other engine does, rather than by
   the oracle.* 4-for-4. The `$meta` defect was a single false claim — "mongod
   result: just `_id`" — copied into both engines and then asserted by **eight
   tests across three layers**. Parity stayed green because both engines were
   wrong identically.
2. *A test whose name or docstring asserts a limitation rather than a
   behaviour.* 3-for-3. Expect to rewrite tests when closing a bug in this class;
   they are pinning it.

**"Flaky" is a description of a bug, not an excuse.** A Windows-only CI failure
that looked environmental was `time.monotonic_ns()` at ~15.6 ms granularity
making `top` report zero timings on that platform. It would have gone green on a
re-run.

## Why this file exists rather than a longer backlog

`tasks/backlog.md` has 76 open items. That number is not a work estimate and
never was: it mixes real defects with scope decisions, upstream driver bugs,
performance work, infra chores, and finished work whose box was never flipped.
The triage block at the top of `backlog.md` classifies every one; this file
sequences the part that is actually work.

Two campaigns were planned off the backlog text in August 2026 and **both were
substantially already done** when measured:

| campaign | backlog said | measured |
|---|---|---|
| "Rust server errors where Python defers" (5 entries) | five porting gaps | **42 of 45 constructs already correct**; of the 3 failures none was Rust-specific |
| SQL/PG defects (10 entries) | ten defects | **2 of 8 sampled already shipped** (CancelRequest, catalog columns) |

That is the single most important input to this plan, and it drives Phase 0.

## Method — the part that should outlive the priorities

Every bug fixed in the August 2026 sweep (eleven of them, six crash-class or
silent wrong-data) was found by **executing against an oracle**, never by
reading. None was accurately described in the backlog beforehand. Four
techniques earned their keep and are reusable:

1. **Three-way differential** (`tests/test_mongod_differential.py`, 53 cases).
   Run the same operation against the Python server, the Rust server and a live
   `mongod`. mongod is the oracle. Untouched surfaces are listed in Phase 2.
2. **A live PostgreSQL oracle.** One runs on the dev box
   (`host=127.0.0.1 port=5432`); `SECANTUS_PG_ORACLE_DSN` points
   `test_subms_predicates_match_real_postgres` at it. Use it for *every* SQL
   claim — it settled six in an afternoon.
3. **Cross-referencing comments (4-for-4).** A Rust comment that justifies
   behaviour by what the *Python* engine does, rather than by mongod, usually
   means the Python behaviour is itself the bug and the Rust deferral exists to
   preserve it. Greps: `"which the pure code"`, `"matching the pure evaluator"`,
   `"Python .* raises"` near an `Err(Fallback)`. The benign form cites mongod in
   the same breath.
4. **Tests that assert a limitation (3-for-3).** A test whose name or docstring
   pins a *limitation* rather than a behaviour is often protecting a bug.
   `test_comparisons_remain_millisecond_blind` said it pinned the gap "so it
   stays visible" — and pinned two wrong answers doing it. Expect to rewrite a
   test when closing this class.

**Corollary, learned twice the hard way: estimates from reading code have been
unreliable; estimates from running it have not.** `_pg_expandarray(...).x` was
called a planner slice and advised against — the inference already existed, and
one level above it sat a bug that made `unnest` fail on *every* non-integer
array. Do not size an item without reproducing it.

---

## Phase 0 — Verify before committing — **COMPLETE (2026-08-28)**

Cheap, and it has twice reclaimed more board than the work it replaced. Nothing
below Phase 1 should start until its item is reproduced.

- [x] **Re-probe the Mongo-side classified items — DONE.** `top` counters had
      already been closed by another session (#1064); its *timings* were broken
      on Windows and are fixed. `$meta` projection turned out **worse than
      recorded** — it discarded the whole document — and is fixed. **The other
      four were probed 2026-08-28 and all four are closed:** the C-driver
      `writeConcernError` (#1071), change-stream `awaitData` with no `maxTimeMS`
      (never a server bug), multi-document update/delete chunking (#795 / #798)
      and the txn dirty-budget guard (#791 / #792). The last two landed
      **2026-08-09** and both entries already said "DONE on both servers" in
      their own body text — the boxes had simply never been flipped, so they read
      as open defects for three weeks. Flipped, with dates, in `backlog.md`.
      **This class is down to two: `$meta` values, and the wrapper prefix.**
      *Original text:* `top` counters, the C-driver
      `writeConcernError` failure, `$meta` projection values, change-stream
      `awaitData` with no `maxTimeMS`, multi-document update/delete chunking
      (Python side), the user-transaction dirty-budget guard (Python side).
      Method: extend the three-way differential. Expect some to be stale.
- [x] **Re-probe the remaining SQL claims — DONE.** 3 stale (deferred
      constraints, catalog remainder, half the partial-index entry), 2 confirmed
      (cross-type lenient pairs, `indpred`), and **3 new bugs** found and fixed
      (`jsonb || jsonb` silent wrong data, `jsonb - key` crash, unsupported
      operand pairs crash). *Original text:*
      `test_return_untyped[b]`, cross-type lenient pairs, the jsonb gap (core
      operators `@> ? ->> #> <@` were verified working — the named gap needs its
      specific case), the pgjdbc and pgx gauge tails, deferred constraints, the
      catalog remainder, partial-index `pg_get_expr`.
- [x] **Re-measure the five "Rust server errors where Python defers" entries —
      DONE (2026-08-26).** 42 of 45 constructs already correct; `$where` /
      `$function` reclassified WONTFIX (need a JS engine). *Original text:*
      Measured ~90% closed. They should probably be rewritten or closed rather
      than worked.

**Output:** a corrected count, and a defect list that is an inventory rather
than a map.

---

## Phase 1 — Confirmed defects, in priority order

Every item here was reproduced against an oracle in August 2026.

### 1a. SQL/PostgreSQL correctness (highest density) — **2 of 7 done**

| item | measured behaviour | note |
|---|---|---|
| ~~`ORDER BY` over a set-returning function~~ **FIXED for `unnest`** (2026-08-28); the record-SRF field form still keeps array order | array order, not sorted; **`ORDER BY <alias> DESC` errors `0A000`** where PG answers | the erroring shape is the one real queries use |
| `_pg_expandarray(...).x` type — **still open**; the attempt surfaced a *wider* bug (`unnest` declared `int4` for every array, failing outright on non-int ones) which **is** fixed | returns **text**, PG returns the element type | record-SRF field projection is typed by a path that assigns `any` *before* `_infer_scalar_tag`; finding that path is the work |
| Write-conflict semantics | second writer gets `40001`; PG blocks and proceeds | clients that treat `40001` as fatal abort |
| COPY OUT abort | transaction stays `INTRANS`; PG gives `INERROR` | needs interleaved client-abort detection |
| `SET search_path` | recorded but ignored in name resolution | |
| Partial-index reflection | `pg_indexes.indexdef` drops the `WHERE` clause | needs a Mongo-filter → SQL-predicate render |
| `ORDER BY` within one millisecond — **still open**; sub-ms *predicates* are fixed (2026-08-27), the sort tiebreaker is not. Plan's cheapest next item | millisecond-granular | the *other half* of the sub-ms entry; predicates are fixed, sorting needs the companion as a tiebreaker |

**Sequencing note.** The two SRF items are adjacent (same subsystem, and the
`.x` path hunt likely surfaces the ORDER BY expansion point). Do them together.
Sub-ms `ORDER BY` is adjacent to work already landed and should be cheap.

### 1b. Mongo-side error fidelity — **1 of 3 done, 1 advanced**

- [x] **Wrong-typed command arguments — DONE, sweep is 87/87 clean.** Was 24
      crashes + 44 divergences; closed across four PRs (#1078 document-valued,
      #1080 numeric/cursor, #1084 the 24 silently-accepted, plus the wrong-code
      slice). Detail and the per-slot lessons are in `backlog.md` §3. **The Rust
      server has not been swept for this class** — point the same probe at
      `secantusd-rs` to find out; that measurement has never been taken.
- [ ] **Aggregation runtime errors lack mongod's wrapper prefix.** Codes match;
      the message doesn't. mongod picks between
      `Failed to optimize pipeline :: caused by ::` and
      `PlanExecutor error during aggregation :: caused by ::` by whether it could
      **constant-fold** the expression — all-literal args fold, any field
      reference doesn't (probed both ways on `$divide` and `$ln`). Closing it
      means modelling constant folding, for message text only.
      **Deliberately deferred**; listed so the analysis isn't redone.
- [ ] **The Rust error-code class — PARTLY ADVANCED.** `update::arith_type_error` shipped as the worked template (2026-08-25); the class is not closed (e.g. `$densify` on a string still answers `BadValue` where mongod says `5733201`). A construct the Rust engine can't do
      surfaces as generic `BadValue` (2) rather than mongod's typed code — e.g.
      `$densify` on a string answers 2 where mongod answers 5733201.
      **`update::arith_type_error` is the worked template**: a standalone
      validator that names the errors it can name, leaving `Fallback` for
      genuinely unimplemented constructs, plus a `StorageError` variant and one
      arm in the adapter's code table. Do *not* widen `Fallback` itself — that
      touches 37 sites and the PyO3 boundary.

---

## Phase 2 — Keep the differential moving — **1 of 5 surfaces done**

The differential's hit rate has been better than working the known list: eleven
bugs from a handful of probes, and the `findAndModify` sweep below kept the rate
up. These surfaces are still **untouched**:

- [ ] Change streams (resume tokens, `fullDocument` modes, invalidation)
- [ ] Index and query planning (`explain` shapes, hint honouring, multikey)
- [x] **`findAndModify` (all option combinations) — DONE 2026-08-29.**
      49 combinations probed against mongod 6.0.16, **14 diverged**, all fixed.
      Method note worth reusing: compare the **raw command reply**, not
      pymongo's `find_one_and_*` wrappers — half the divergences were in the
      reply *shape* (`lastErrorObject`'s keys, the field order of an upserted
      document), which the wrappers hide.
      Two were silent wrong data and **both were shared with the plain
      `update` command**, so probing one command found bugs in two: `update: {}`
      is a replacement (mongod reduces the doc to its `_id`; we returned it
      untouched) and an upsert from a dotted query (`{"sub.k": 77}`) stored a
      literal dotted key that then failed to match its own query. A third,
      `$set: {"n.x": 1}` against `{n: 5}`, silently did nothing where mongod
      answers `PathNotViable` (28) — and the Rust port carried a
      "Python walk returns None -> no-op" comment, making it the **fifth** hit
      for the cross-referencing-comments pattern (now 5-for-5).
      The rest were arguments accepted and ignored (`new` untyped, unknown
      top-level fields, `hint`) or error codes flattened to `14 TypeMismatch`
      on the way out of the command. `tests/test_mongod_differential.py`
      57 → 93 cases.
- [ ] Cursor / `getMore` semantics (batch sizing, `maxTimeMS`, tailable)
- [ ] `$lookup` / `$graphLookup` forms (`let`/`pipeline`, nested)

Caveat worth stating plainly: this **adds** to the board rather than shrinking
it. That is a feature — undiscovered wrong answers are worse than known ones —
but it does not make the count go down.

Two message-only gaps were opened rather than closed by the findAndModify
sweep; both are recorded in `backlog.md` and neither changes a code:

- `hint` that names no index answers `BadValue` (2) with our short causal
  sentence, where mongod prefixes a dump of the parsed plan (`error processing
  query: ns=…Tree: _id $eq 1\nSort: {}\nProj: {}\n…`). Shared with `find` and
  `count`, which have always answered the short form.
- A `$`-prefixed unknown field (`$zz`) is accepted where mongod rejects it.
  Deliberate: `$`-keys are the wire envelope, and the `create` command makes
  the same carve-out.

---

## Explicit non-goals

Not work, and should not be counted as such. See the triage block for the full
classification.

- **WONTFIX (12):** multi-node machinery, alternative auth mechanisms
  (LDAP/Kerberos/GSSAPI/AWS/OIDC), `$where` / `$function` (need a JavaScript
  engine — reclassified 2026-08-27 after being mistaken for a porting gap),
  BSON representation limits (sub-ms dates, `numeric` beyond 34 digits),
  ephemeral-db connection semantics, macOS x86_64 wheels.
- **UPSTREAM (5):** Java / Go / Ruby gauge failures that are driver or
  test-harness artifacts.
- **PERF (11):** the mongod throughput comparison and the WiredTiger levers.
  Real, but a different class from correctness — do not mix them into a
  correctness tranche.
- **CHORE (9):** CI, release, one-time configuration.

---

## How this ages

The **evidence** here is dated and durable: a measurement against mongod 6.0.16
or PostgreSQL 14 on a stated date stays true about that date. The
**priorities** are a snapshot and will rot.

`tasks/RESUME.md` opens with "STALE SNAPSHOT … ages by design" for exactly this
reason, and there are 31 plan files in this directory. To avoid becoming the
32nd:

- When an item here is closed, **delete its row** and note the fix in
  `backlog.md` — do not leave a checked box here and an open one there.
- When Phase 0 finds an item stale, **correct `backlog.md` first**; this file
  points at the backlog, not the other way round.
- If this file and `backlog.md` disagree, `backlog.md` wins.
- If nothing here has been touched in a release cycle, it has rotted — re-run
  Phase 0 rather than trusting it.
