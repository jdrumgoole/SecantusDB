# Retargeting the error surface from mongod 6.0 to 8.x — PARKED

> **STATUS: PARKED (2026-08-29), awaiting coordination. Do not resume without
> reading "Why it is parked".** Branch `retarget-mongod8`, pushed. The suite on
> this branch is **RED BY CONSTRUCTION** (51 failures) — that is the expected
> mid-flight state, not a regression to debug.

## Why this exists

`tests/test_mongod_differential.py` compares SecantusDB against whatever `mongod`
is on PATH and asserts **exact** equality. The codebase is probed against
**6.0.16** (49 citations across 10 source files, including error strings copied
verbatim *with* 6.0's malformed closing quote). On a box with **8.2.1** that
produced 23 failures that were version differences, not divergences.

PR #1103 gated the gate on `PROBED_MONGOD_SERIES` so it skips off 6.0, and left
the real decision open: stay on 6.0 deliberately, or retarget. **The decision
taken was: retarget to a supported server.** This branch is that work, stopped
partway.

## Why it is parked — READ THIS FIRST

**A parallel session is actively adding 6.0-pinned fidelity tests while this
branch retargets away from 6.0.** All six files this branch breaks were
committed to within the preceding three days:

| file | commits in prior 3 days |
|---|---:|
| `tests/test_arg_types_accepted_slots.py` | 3 |
| `tests/test_findandmodify_fidelity.py` | 2 |
| `tests/test_arg_types_numeric.py` | 1 |
| `tests/test_update_execution_errors.py` | 1 |
| `tests/test_cursor_argument_fidelity.py` | 1 |
| `tests/test_hint_and_explain_fidelity.py` | 1 |

Resuming while that campaign runs means continuously rewriting assertions the
other session is still adding. **Coordinate first:** either that campaign
finishes (or pauses), or this stays parked. This is the blocker, not the
remaining code.

## Where it got to

Differential gate against a live mongod 8.2.1: **23 failures → 8.** Four
families retargeted, each verified against the running server:

| family | 6.0.16 | 8.2.1 | cases |
|---|---|---|---:|
| negative cursor sizing | `51024 Location51024` | `2 BadValue` | 5 |
| expected-type lists | `'[bool, long, int, decimal, double']` | reordered, quote fixed | 3 |
| `distinct` IDL struct | `distinct.zz` | `distinctCommandRequest.zz` | 1 |
| update executor wrapper | bare message | `Plan executor error during update :: caused by :: ` | 6 |

The type-set **ordering is per field and can only be probed** — `getMore
.batchSize`, `findAndModify.remove` and `$densify.range.step` all disagree
(`[decimal, int, double, long]`, `[int, decimal, long, bool, double]`,
`[double, long, int, decimal]`). `$densify.range.step` had no differential case,
so it was probed directly rather than guessed. **Do not derive these.**

The update wrapper was cheap because `UpdateError.exec_error` already existed and
`findAndModify` already wrapped — on 6.0 it was alone in doing so.

## What remains

1. **8 differential cases**, the hard ones:
   - **`$lookup` (5)** — needs its argument validation reshaped to IDL-style:
     missing `as` `9` → `40414 "BSON field '$lookup.as' is missing but a required
     field"`; unknown arg `9` → `40415`; `let` wrong type `9` → `14 "BSON field
     '$lookup.let' is the wrong type 'int', expected type 'object'"`; pipeline
     wrong type → `14 'A pipeline must be an array of objects'`; half-specified
     pair wording.
   - **densify / aggregate (1)** — needs an `exec_error` flag on
     `AggregateError` (mirroring `UpdateError`) plus namespace-aware wrapping
     (`Executor error during aggregate command on namespace: <ns> :: caused by
     :: `). Aggregate errors currently reach `dispatch`'s generic handler, which
     knows neither the namespace nor parse-vs-execution — so this needs a catch
     in the aggregate command handler, not a blanket wrap.
   - **null arguments (2)** — `findAndModify.arrayFilters: null` must be treated
     as *absent* (8.x succeeds; we return `10065`); `killCursors.cursors: null`
     → `40414 "BSON field 'killCursors.cursors' is missing but a required
     field"`.
2. **43 existing tests** rewritten from 6.0 to 8.x expectations (the six files
   above). This is the bulk of the work and the whole collision.
3. **Rust mirror** — `crates/secantus-commands/src/argtypes.rs` carries both type
   constants; `src/admin.rs` carries `51024` including its own unit assertion at
   `admin.rs:1875`. Both servers must move together or Rust must defer.
4. **~49 probe comments** citing 6.0.16. **Re-probe; do not blanket-rewrite** —
   claiming a probe that was never run is worse than a stale citation. Many of
   those sites are version-stable and need only a reworded comment.
5. **Gauges** — pymongo + the driver gauges must not regress. Risk looks
   contained (`grep` found no `51024` / `Location51024` in `vendor/`), but that
   is unproven.
6. **Flip `PROBED_MONGOD_SERIES`** to the target series and decide granularity:
   it currently compares `(major, minor)`, so `(8, 2)` means an 8.0 box skips.
   Note CI installs mongosh + database-tools but **not `mongod`**, so this gate
   never runs in CI at all.

## Resuming

The branch is pushed; a draft PR keeps the claim visible. A fresh worktree
**needs its own venv** — `src` is changed here, and a borrowed venv silently
resolves `secantus` from whatever checkout its editable install points at, so
edits appear to do nothing (this cost a full debug cycle):

```bash
git worktree add ../SecantusDB-retarget-mongod8 retarget-mongod8
cd ../SecantusDB-retarget-mongod8
git submodule update --init vendor/wiredtiger
uv sync --extra dev --extra admin        # builds WiredTiger, several minutes
./.venv/bin/python -m pytest -q tests/test_mongod_differential.py
```

The differential gate with `PROBED_MONGOD_SERIES = (8, 2)` on a mongod 8.2.1 box
is the red/green loop — it uses the real server as the oracle, which is the only
honest way to get these strings right.
