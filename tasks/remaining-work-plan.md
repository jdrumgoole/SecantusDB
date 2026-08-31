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

* `mongod` is on `PATH` — **it reports 8.2.1 as of 2026-08-31**, not the 6.0.16
  this file was written against, and not the 8.2.11 this line claimed until
  2026-08-31; the error surface was retargeted to 8.x on 2026-08-29 (see
  `CLAUDE.md`). **8.2.11 is installed separately** at
  `/opt/homebrew/opt/mongodb-community@8.2.11/bin/mongod`; put it first on
  `PATH` to probe against it. Measurements below dated against 6.0.16 stay true
  about *that* version — re-probe before relying on one, and note that 6.0.16
  and 8.3.4 are no longer installed at all. Start a server on
  `127.0.0.1:27019` and diff against it. `tests/test_mongod_differential.py` is
  the standing harness; run it with `-m differential`.

  **Run `mongod --version` and record it with the measurement.** This line was
  wrong for a day because nobody did, and a *patch* bump has changed an error
  message here before — "8.x" is not specific enough to cite.
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
| ~~`ORDER BY` over a set-returning function~~ **FULLY FIXED** — `unnest` 2026-08-28, the record-SRF field form 2026-08-29 | array order, not sorted; the alias form errored `0A000`/`42703` | the ordinal form was ACCEPTED and silently unsorted — the worse half |
| ~~`_pg_expandarray(...).x` type~~ **FIXED 2026-08-29** — `.x` types as the array's ELEMENT (int4 / text / numeric), was `any` / oid 0 for every array | returns **text**, PG returns the element type | the argument's own result column already carried the array tag |
| Write-conflict semantics — **RE-MEASURED 2026-08-29, entry was wrong both ways**: autocommit already MATCHES PG (blocks, both writes land); only explicit txns diverge, because they run snapshot isolation (= PG's REPEATABLE READ). An unretried client LOSES a write, and SERIALIZABLE permits write skew | second writer gets `40001`; PG blocks and proceeds | true READ COMMITTED needs per-statement snapshots — a redesign, not a slice. SERIALIZABLE policy is a product decision. Pinned by `tests/test_sql_isolation_level.py` |
| COPY OUT abort — **DOES NOT REPRODUCE (2026-08-29)**: probed twice, both PG and we leave the transaction `INTRANS`. Re-probe with the exact client-abort shape before working it, or close it | transaction stays `INTRANS`; PG gives `INERROR` | needs interleaved client-abort detection |
| ~~`SET search_path`~~ **FIXED 2026-08-29** — order decides, off-path relations are invisible, and CREATE targets the path's first schema | recorded but ignored in name resolution | two tests pinned the old behaviour and were rewritten |
| ~~Partial-index reflection~~ **FIXED 2026-08-29** — the stored filter is reversed back to SQL; 10 predicate shapes render byte-identically to PG (incl. `b <> 5` through its `$and` desugaring, and the `::text` cast) | `pg_indexes.indexdef` drops the `WHERE` clause | unrecognised shapes still omit the WHERE rather than approximate it |
| ~~`ORDER BY` within one millisecond~~ **FIXED 2026-08-29** — two code paths (the plain-column sort key and the projected-expression scope); LIMIT and DISTINCT ON had been returning wrong *rows*, not merely wrong order | millisecond-granular | what remains of sub-ms is pipeline-shaped reads (GROUP BY / JOIN / DISTINCT); `min`/`max` fixed 2026-08-29 |
| ~~Result types that disagree with their values~~ **FIXED 2026-08-30** — jsonb `\|\|` / `-` lost the jsonb tag, and unknown-literal arithmetic widened instead of resolving to the other operand's type | right value, wrong declared OID → the CLIENT raised decoding it | found by comparing the **result OID** over the wire against live PG; an in-process probe of the same statements showed nothing |

**Sequencing note.** The two SRF items are adjacent (same subsystem, and the
`.x` path hunt likely surfaces the ORDER BY expansion point). Do them together.
Sub-ms `ORDER BY` is adjacent to work already landed and should be cheap.

### 1b. Mongo-side error fidelity — **1 of 3 done, 1 advanced**

- [x] **Wrong-typed command arguments — DONE, sweep is 87/87 clean.** Was 24
      crashes + 44 divergences; closed across four PRs (#1078 document-valued,
      #1080 numeric/cursor, #1084 the 24 silently-accepted, plus the wrong-code
      slice). Detail and the per-slot lessons are in `backlog.md` §3.
      **The Rust server HAS been swept — twice, and this paragraph was stale
      when it said otherwise** (`tasks/backlog.md` wins per `CLAUDE.md`, and it
      recorded the first sweep on 2026-08-29: 78 of 87 divergent → clean, then
      244/244). The hint that "the class ports across wholesale" was right: it
      did.

      **The sweep's own reach was the real gap.** `arg_types_extended.py`
      compares CODES only. Comparing MESSAGES over 685 shapes on 2026-08-31
      found **76 further slots** divergent on the Rust server, almost all
      silently accepted — closed, with the residue recorded in `backlog.md`.
      **The PYTHON server is now the open half of this item**: same 685 shapes,
      ~61 slots, and **18 crashes** answering `internal server error` from an
      `int()` over a wrong-typed value — the crash class #1080 closed, on the
      slots its corpus never reached.
- [ ] **Aggregation runtime errors lack mongod's wrapper prefix — STILL OPEN in
      general, but the framing "message text only" was WRONG and cost a real
      bug three weeks (measured 2026-08-31 against 8.2.11).** mongod picks
      between `Failed to optimize pipeline :: caused by ::` and
      `PlanExecutor error during aggregation :: caused by ::` by whether it could
      **constant-fold** the expression — all-literal args fold, any field
      reference doesn't (probed both ways on `$divide` and `$ln`).

      **Folding is not cosmetic where the folded expression is itself the
      error.** A `$switch` with only constant cases and no `default` is rejected
      during optimization, so mongod answers 40069 **over an empty collection**,
      with no document ever read. We returned an empty cursor and `ok: 1` — the
      "argument accepted and ignored" shape — and only failed once a document
      happened to exist. That is a wrong ANSWER, not wrong wording, and it hid
      here because the entry said the gap was message text. **Fixed 2026-08-31**
      (`aggregate._fold_constant_switches`), along with the execution-time arm
      (40066, was a generic `14 TypeMismatch`).

      What remains deferred is genuinely message-only: modelling folding for
      operators whose *arguments* fold (`$divide`, `$ln`) so the right one of
      the two prefixes is chosen. Before deferring the next one, check whether
      the construct being folded can itself raise — if it can, the divergence
      reaches the answer.

      Method note: probe against an **empty collection**. That is the single
      cheapest way to separate a parse/optimize-time error from an
      execution-time one, and comparing populated collections alone reports the
      two as merely differing in wording.
- [ ] **The Rust error-code class — SPEC-LEVEL HALF DONE (2026-08-30), runtime
      half open.** A construct the Rust engine can't do surfaces as generic
      `BadValue` (2) rather than mongod's typed code.

      **What closed, and how, because it was cheaper than this entry assumed.**
      Validating the stage SPEC at the command layer — where `$facet` already
      validates its own — gives mongod's code without touching the engine's
      error type at all. That closed seven cases: `$setWindowFields` unknown
      field / missing output / unknown window key (40415 / 40414 / 9), `$sample`
      negative and non-numeric size (28747 / 28746), `$unwind` unprefixed path
      (28818), `$bucket` one-element boundaries (40192), `$densify` non-positive
      step (5733401) and `$fill` unknown method (6050202). None of it needed the
      `Fallback` widening this entry warned about.

      **What remains is the RUNTIME half** — errors discoverable only while
      processing documents, which no spec check can reach. Measured on the Rust
      server against mongod 8.2.11 on 2026-08-30:

      | case | rust | mongod |
      |---|---|---|
      | `$densify` on a string field | 2 | 5733201 |
      | `$bucket` with no `default` and an out-of-range value | 2 | 7158303 |
      | `$replaceRoot` whose `newRoot` resolves to a scalar | 2 | 40228 |

      **The PYTHON server was measured on the same three cases 2026-08-30
      (mongod 8.2.11 on `:27019`, Python server on `:27018`) and is NOT uniformly
      wrong — only one of the three needs a code at all:**

      | case | python code | python message | verdict |
      |---|---|---|---|
      | `$densify` on a string field | 5733201 | wrapped, text matches | **already correct** |
      | `$bucket` with no `default` | 7158303 | correct text, **wrapper prefix missing** | message half only |
      | `$replaceRoot` scalar `newRoot` | **14** (want 40228) | `$replaceRoot newRoot must evaluate to a document`, **unwrapped**; mongod says `'newRoot' expression  must evaluate to an object, but resulting value was: 1. Type of resulting value: 'int'. Input document: {n: 1}` (mongod's own double space after `expression`) | code + message |

      **The Python half is DONE (2026-08-31).** All three cases above now match
      8.2.11 exactly, plus `$replaceWith`, and 30 cases pin it in the
      differential gate (`AGGERR_CASES`). Two corrections to the analysis below,
      both found by running it:

      * The `$switch` case was recorded as needing only `exec_error=True`. It
        already had the wrapper (an `ExpressionError` always qualifies); what it
        lacked was mongod's code (40066) and wording — and, in the foldable
        form, a parse-time rejection it never performed at all (see the entry
        above).
      * `$replaceRoot`'s message embeds the **dependency-pruned** input
        document, not the stored one: `{_id: 1, n: 1, s: "hi"}` with
        `newRoot: "$n"` reports `{n: 1}`. Field order follows the DOCUMENT, a
        referenced parent subsumes a referenced child, an absent path is
        omitted, and `$$ROOT.a` counts as a path read. None of that is derivable
        from the code; all of it came from the oracle.

      *Original analysis, kept because its shape was right:*
      `AggregateError` already takes `code=` and `exec_error=True`, and
      `commands.py` (the `apply_pipeline` except arm) already applies mongod's
      `Executor error during aggregate command on namespace: <ns> :: caused by ::`
      prefix to any `exec_error` raise — which is exactly why `$densify` is
      already right. So: `aggregate.py`'s `$replaceRoot newRoot must evaluate to
      a document` raise needs `code=40228, exec_error=True` and mongod's text,
      and the `$switch` no-matching-branch raise needs `exec_error=True`. This
      entry's cost estimate was written from the Rust side and over-reads the
      Python side.

      For the Rust side the template still applies: **`update::arith_type_error`** — a standalone
      validator naming the errors it can name, leaving `Fallback` for genuinely
      unimplemented constructs, plus a `StorageError` variant and one arm in the
      adapter's code table. Do *not* widen `Fallback` itself — that touches 37
      sites and the PyO3 boundary.

      Method note: compare error MESSAGES, not just codes. Comparing codes alone
      reported the Python server correct on five cases that had mongod's code
      with different wording (`$sample` said "must not be negative" for "must be
      a positive integer"; `$bucket` dropped "value(s)").
---

## Phase 2 — Keep the differential moving — **COMPLETE (5 of 5, 2026-08-29)**

The differential's hit rate has been better than working the known list: eleven
bugs from a handful of probes, and the `findAndModify` sweep below kept the rate
up. **All five surfaces are now done** (2026-08-29):

- [x] **Change streams (resume tokens, `fullDocument` modes, invalidation) —
      DONE 2026-08-29 — swept TWICE, for different things (#1104 and this
      slice).** mongod refuses change streams on a standalone and the
      differential harness spawns one, so this surface had never been compared
      against a real server at all; both sweeps had to stand up a single-node
      replica set first.

      **Arguments (#1104):** 13 shapes, **all 13 diverged.** One crash (a resume
      token that is valid hex but not valid BSON), and the rest overwhelmingly
      arguments ACCEPTED AND IGNORED — the worst shape for a stream
      specifically, because the caller believes they asked for something: a
      misspelled `fullDocument: "updateLookup"` produced a stream without
      lookups, a wrong-typed `resumeAfter` one that restarted from the
      beginning. Both reported success.
      Method note: the validation belongs in `changestreams.parse_spec`, NOT in
      the `$changeStream` pipeline stage — the `aggregate` command routes change
      streams through its own path and never runs that stage, so a first attempt
      in the stage handler had no effect at all.

      **Events and errors (this slice):** 41 cases, 14 diverged. The `required`
      pre-/post-image error answered 280 where mongod answers **47
      `NoMatchingDocument`** with no error labels — two distinct conditions had
      been sharing one exception class, so one wrong code covered both; the
      stripped-resume-token error was missing mongod's `Executor error during
      getMore` wrapper and its `Expected: … but found: …` tail; and events were
      emitted with `wallTime` and `fullDocument` in the wrong positions, with
      `_id` not first. **Field order is invisible to a document comparison**,
      which is how nine construction sites drifted unnoticed and why the probe
      now compares key lists as its own pass — that alone found 28 of 34 CRUD
      events out of order. Fixed on both servers, which now diverge from mongod
      on exactly the same remaining cases.
      Recorded, not fixed: `truncatedArrays` (mongod never emits it, measured
      across eight mutations and four array sizes — its own slice, ~19
      assertions across two mirrored engines) and the expanded-event
      `collectionUUID` / `stateBeforeChange` gaps.
- [x] **Index and query planning (`explain` shapes, hint honouring, multikey)
      — behavioural half DONE 2026-08-29; `explain`'s stage vocabulary
      deliberately left whole (one scoped item in `backlog.md`).**
      32 shapes probed. The find that matters: **`delete` and `update` ignored
      their per-statement `hint` and PERFORMED the write** where mongod refuses
      the statement — the only write-that-should-not-have-happened in the whole
      campaign, and the two write paths were the only hint-bearing commands
      still missing the check. Also: `explain` did not validate a hint (the
      command you run to *check* one) and **fabricated** a `COLLSCAN` plan with
      `ok: 1` for commands that do not exist; `{$natural: -1}` did not reverse;
      `distinct` accepted any field.
      **Read the raw count carefully** — 30 of 32 "diverged", but most is
      `explain` output shape and one case is not a defect: we choose `a_-1`
      where mongod chooses `a_1` for `{a: 5}`, which is a cost model, not a
      wrong answer. Six behavioural items were the content. A differential
      number is only as meaningful as its classification.
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
- [x] **Cursor / `getMore` semantics (batch sizing, `maxTimeMS`, tailable) —
      DONE 2026-08-29.** 51 shapes probed, **22 diverged, four crash-class**: a
      malformed argument reached a bare `int()` and the exception escaped as
      `internal server error` (code 1). The pattern across the rest: `getMore`
      answered `CursorNotFound` (43) for four different *parse* errors — about
      a cursor that existed — where mongod reports the parse error before it
      looks a cursor up, and negative `batchSize` / `limit` / `skip` were
      accepted everywhere (a negative `batchSize` fell through `or DEFAULT` and
      became the default; a negative `limit` returned the whole collection).
      **The Rust server does NOT share the crashes** — its `as_i64` returns
      `None` where Python's `int()` raises — but it does share the
      accepted-and-ignored family; that belongs to the standing "point the same
      probe at `secantusd-rs`" item in 1b and is not closed here.
      One test was pinning the permissive behaviour, the third instance this
      week: `test_tailable_drop_closes_pymongo_cursor_cleanly` set
      `max_await_time_ms` on a plain `TAILABLE` cursor, which real mongod
      rejects on the getMore. pymongo sends it anyway — its guard is a bitmask
      test (`flags & CursorType.TAILABLE_AWAIT` is `2 & 6 == 2`, truthy) — so
      the option it documents as ignored reaches the wire.
- [x] **`$lookup` / `$graphLookup` forms (`let`/`pipeline`, nested) — DONE
      2026-08-29 (#1102).** 27 shapes, 20 diverged. The worst was a SHORT ANSWER
      WITH NO ERROR: `$graphLookup` stopped following the chain at the first
      null `connectFromField`, so a four-document chain returned one document.
      Also: an empty-array `localField` matched nothing where mongod joins it
      against null-valued rows (BOTH join paths, and the index path's comment
      claimed `$in: []` semantics the oracle contradicts); `as` treated as a key
      rather than a path; two crashes; unknown arguments accepted.
      **Correction kept deliberately**: first written up as "does not recurse at
      all" and scoped as a possible feature build. It recurses — the fixture put
      a null on the first hop. The fix was a guard.

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
