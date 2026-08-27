# Remaining work — a plan built on measurement, not on the backlog text

> **Written 2026-08-27.** Read the "How this ages" section at the bottom before
> trusting anything below. The dated *evidence* here is durable; the *priorities*
> are a snapshot.

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

## Phase 0 — Verify before committing (do this first)

Cheap, and it has twice reclaimed more board than the work it replaced. Nothing
below Phase 1 should start until its item is reproduced.

- [ ] **Re-probe the Mongo-side classified items.** `top` counters, the C-driver
      `writeConcernError` failure, `$meta` projection values, change-stream
      `awaitData` with no `maxTimeMS`, multi-document update/delete chunking
      (Python side), the user-transaction dirty-budget guard (Python side).
      Method: extend the three-way differential. Expect some to be stale.
- [ ] **Re-probe the remaining SQL claims** against the live PostgreSQL:
      `test_return_untyped[b]`, cross-type lenient pairs, the jsonb gap (core
      operators `@> ? ->> #> <@` were verified working — the named gap needs its
      specific case), the pgjdbc and pgx gauge tails, deferred constraints, the
      catalog remainder, partial-index `pg_get_expr`.
- [ ] **Re-measure the five "Rust server errors where Python defers" entries.**
      Measured ~90% closed. They should probably be rewritten or closed rather
      than worked.

**Output:** a corrected count, and a defect list that is an inventory rather
than a map.

---

## Phase 1 — Confirmed defects, in priority order

Every item here was reproduced against an oracle in August 2026.

### 1a. SQL/PostgreSQL correctness (highest density)

| item | measured behaviour | note |
|---|---|---|
| `ORDER BY` over a set-returning function | array order, not sorted; **`ORDER BY <alias> DESC` errors `0A000`** where PG answers | the erroring shape is the one real queries use |
| `_pg_expandarray(...).x` type | returns **text**, PG returns the element type | record-SRF field projection is typed by a path that assigns `any` *before* `_infer_scalar_tag`; finding that path is the work |
| Write-conflict semantics | second writer gets `40001`; PG blocks and proceeds | clients that treat `40001` as fatal abort |
| COPY OUT abort | transaction stays `INTRANS`; PG gives `INERROR` | needs interleaved client-abort detection |
| `SET search_path` | recorded but ignored in name resolution | |
| Partial-index reflection | `pg_indexes.indexdef` drops the `WHERE` clause | needs a Mongo-filter → SQL-predicate render |
| `ORDER BY` within one millisecond | millisecond-granular | the *other half* of the sub-ms entry; predicates are fixed, sorting needs the companion as a tiebreaker |

**Sequencing note.** The two SRF items are adjacent (same subsystem, and the
`.x` path hunt likely surfaces the ORDER BY expansion point). Do them together.
Sub-ms `ORDER BY` is adjacent to work already landed and should be cheap.

### 1b. Mongo-side error fidelity

- [ ] **Aggregation runtime errors lack mongod's wrapper prefix.** Codes match;
      the message doesn't. mongod picks between
      `Failed to optimize pipeline :: caused by ::` and
      `PlanExecutor error during aggregation :: caused by ::` by whether it could
      **constant-fold** the expression — all-literal args fold, any field
      reference doesn't (probed both ways on `$divide` and `$ln`). Closing it
      means modelling constant folding, for message text only.
      **Deliberately deferred**; listed so the analysis isn't redone.
- [ ] **The Rust error-code class.** A construct the Rust engine can't do
      surfaces as generic `BadValue` (2) rather than mongod's typed code — e.g.
      `$densify` on a string answers 2 where mongod answers 5733201.
      **`update::arith_type_error` is the worked template**: a standalone
      validator that names the errors it can name, leaving `Fallback` for
      genuinely unimplemented constructs, plus a `StorageError` variant and one
      arm in the adapter's code table. Do *not* widen `Fallback` itself — that
      touches 37 sites and the PyO3 boundary.

---

## Phase 2 — Keep the differential moving

The differential's hit rate has been better than working the known list: eleven
bugs from a handful of probes. These surfaces are **untouched**:

- [ ] Change streams (resume tokens, `fullDocument` modes, invalidation)
- [ ] Index and query planning (`explain` shapes, hint honouring, multikey)
- [ ] `findAndModify` (all option combinations)
- [ ] Cursor / `getMore` semantics (batch sizing, `maxTimeMS`, tailable)
- [ ] `$lookup` / `$graphLookup` forms (`let`/`pipeline`, nested)

Caveat worth stating plainly: this **adds** to the board rather than shrinking
it. That is a feature — undiscovered wrong answers are worse than known ones —
but it does not make the count go down.

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
