---
name: differential-probe
description: Find real bugs in SecantusDB by running operations against a live mongod (or PostgreSQL) and diffing the answers, instead of reading code or the backlog. Fires when the user asks to find bugs, check conformance/fidelity for a command or stage, work a backlog item, extend tests/test_mongod_differential.py, or asks "does X match mongod?". Holds the probe recipe, the normalisation rules that stop false positives, the mongod-version hazards that only CI reveals, and the recurring bug shapes worth grepping for.
---

# Probe against the oracle; don't reason about the source

**Every bug found in the 2026-08 sweeps came from executing behaviour and
comparing against a real server.** None was accurately described in
`tasks/backlog.md` beforehand; several entries described work as remaining that
was already done, or missing that already existed. Reading is for *fixing*, not
for *finding* and not for *sizing*.

Two oracles live on this box and both are cheap to start:

- `mongod` on `PATH` (Homebrew `@6.0`, reports 6.0.16). Change streams need a
  replica set — spawn with `--replSet rs0` and `replSetInitiate`.
- PostgreSQL 14 at `host=127.0.0.1 port=5432 dbname=postgres user=jdrumgoole`
  for anything SQL-side.

The standing gate is `tests/test_mongod_differential.py` (`pytest -m
differential`). A probe that finds something belongs there, or the next session
re-finds it.

## The recipe

1. **Pick a surface with many option combinations** — a command's arguments, a
   stage's spec, an error path. Divergence density is highest where options
   multiply, not where the happy path lives.
2. **Write a throwaway probe** in the scratchpad: a list of
   `(name, seed, operation)` cases, run each against both servers, print only
   the ones that differ. Keep each case minimal so a failure names one
   behaviour.
3. **Compare the RAW command reply, not the driver wrapper.** Half of what
   diverged in the `findAndModify` sweep was reply *shape* —
   `lastErrorObject`'s keys, an upserted document's field order — which
   `find_one_and_*` hides. Use `db.command({...})`.
4. **Classify honestly before fixing.** A raw divergence count flatters the
   work. The query-planning sweep was "30 of 32 diverged", but most was
   `explain` output shape and one case was not a defect at all (we pick a
   different index than mongod for the same query — identical documents, just a
   different cost model). Six behavioural items were the real content. Say so.
5. **Promote what you fixed into the gate**, plus the neighbouring behaviour
   that was already right — so the fix cannot over-reach later.

## Normalisation: what you must strip, or you get false positives

- **Cluster-time gossip.** SecantusDB advertises a replica set and attaches
  `$clusterTime` / `operationTime` to every reply; a standalone `mongod` does
  not.
- **Per-server identifiers.** Cursor ids, upsert-generated `ObjectId`s, resume
  tokens, collection UUIDs, wall clocks. Compare an id as
  `type + zero-or-open`, which is what drivers actually assert on; replace an
  id echoed inside an error message with a marker.
- **Known-missing wrappers.** mongod prefixes some errors
  (`PlanExecutor error during aggregation :: caused by ::`); strip it on both
  sides so the case asserts the code and message that matter.

## mongod versions differ, and only CI shows it

The gate runs against whatever `mongod` is on `PATH`. **The dev box and the CI
Windows runner do not agree**, and two cases were caught this way — neither
reproducible locally, neither a flake:

- **`codeName` for high numeric codes is not stable.** 40415 is
  `Location40415` on 6.0.16 (mongod's fallback for a code with no symbolic
  name) and `IDLUnknownField` on newer servers. Same code, same message. The
  *named* codes (2, 9, 14, 28, 40, 66) are stable. Assert the name only below
  code 10000.
- **An upserted document's query-seeded field order changed.** 6.0.16 sorts
  them; newer keeps the query's own order. `_id` leading is stable on both.

Before asserting anything version-shaped — a `codeName` above 10000, BSON field
order, whether an option exists at all (`distinct.hint` was added after 6.0) —
either probe an always-stable variant instead, or don't assert that part. The
project convention is to **ship 6.0's form** and gate the assertion.

## Size the work from a probe, never from the source

**Estimates from reading code have been wrong four times, every one in the
expensive direction:**

- `_pg_expandarray(...).x` was called a planner slice and advised against — the
  inference already existed, and a wider bug sat one level above it.
- Two campaigns planned off backlog text were already substantially done when
  measured (42 of 45 Rust constructs; 2 of 8 SQL claims).
- `$graphLookup` was written up as "does not recurse at all" and scoped as a
  possible feature build. **It recurses.** The fixture happened to put a null
  link on the first hop. The fix was a one-line guard.
- The missing-vs-null fix was sized as "a sentinel in `expressions.py`, which
  touches every operator". The sentinel already existed; only the comparison
  operators failed to use it.

An unlucky fixture in one dimension turns a narrow bug into what looks like a
week of work. **Vary the fixture before believing a big diagnosis.**

## Three bug shapes worth grepping for

Each has produced multiple real defects:

- **A user-supplied path used as a dict key** (3 instances: the upsert seed,
  `$lookup`'s `as`, `$graphLookup`'s `as`). Produces a document with a literal
  dot in a key — one mongod cannot make, which then fails to match the query
  that created it. Anything user-supplied reaching a key needs `set_path`.
- **A comment justifying behaviour by something other than the oracle**
  (6 instances). Greps: `"which the pure code"`, `"matching the pure
  evaluator"`, `"Python .* raises"` near an `Err(Fallback)`, and any comment
  asserting a mongod rule. The benign form cites mongod *and* is right; the
  malignant form cites the other engine, or states a mongod rule a probe
  contradicts. One such comment was trusted enough to write a test from, and
  the test failed against the real server.
- **Missing conflated with null** (3 instances). `get_path` returns `None` for
  both; `has_path` distinguishes them. mongod's rule **differs by language**:
  the *query* language treats them alike (`{a: null}` matches a missing field),
  the *expression* language does not (`$eq: ["$absent", null]` is false, and a
  missing field ranks below every value including MinKey).

## Probing one command finds bugs in its siblings

Both silent-wrong-data bugs in the `findAndModify` sweep lived in the shared
update path and were only *found* through `findAndModify`. The hint bug was the
same from the other direction: `find` / `count` / `aggregate` already validated
hints, so only the two *write* commands were left. **When a probe finds
something, re-run its shapes through the neighbouring commands before calling
it fixed.**

## Parity is not correctness

The Rust parity suites pin the two engines to each other, so they are equally
satisfied by **both being wrong** — which has happened (`$bucket` empty
buckets, `$densify` on null, `$stdDev*` on non-numeric). Parity catches drift
within seconds of a change; only the oracle says which side to move. Use both:
after changing an engine, run the oracle probe *and* `pytest -k parity`.

## Before you trust a green suite

- **Compare the count to the last known-good run.** A worktree venv missing
  `_secantus_core` silently uncollects ~1700 parity tests and still exits 0.
  Provision it (`uvx maturin build --release --manifest-path
  crates/secantus-core-py/Cargo.toml`, then `uv pip install --reinstall`) and
  check the number.
- **Run the FULL suite, not the targeted files you reasoned about.** The
  missing-vs-null change broke six SQL outer-join tests three subsystems away —
  `COUNT(col)` must skip SQL NULLs, and an unmatched outer-join row represents
  that NULL as an *absent* field. Every targeted run was green.

## Record it honestly

- Fix the entry, don't add a newer one beside it. An entry that outlives its
  bug advertises finished work as remaining.
- Keep a wrong estimate in the record with the correction; the error is more
  reusable than the fix.
- Divergences you decline to fix (deliberate hardening, version-dependent
  behaviour, cosmetic ordering) go in `tasks/backlog.md` **with the reason**,
  so the next session reads a decision rather than an oversight.
