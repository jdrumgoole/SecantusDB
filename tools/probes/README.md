# Differential probes

Small scripts that run the same operations against **SecantusDB and a real
mongod** and print only the disagreements. They are not tests — they are the
tool you reach for when you want to find out what is wrong in an area nobody has
checked, rather than to prove a known thing stays fixed.

`tests/test_mongod_differential.py` is the standing gate; these are the
exploratory front end that feeds it. Anything a probe finds should end up as a
regression test there or in a dedicated test file.

## The SQL side: `pg_differential.py`

The same idea against **PostgreSQL** instead of mongod, for SecantusDB's PG
server. It takes a corpus from `pg_corpora/` (a `.setup.sql` plus the statements
proper) and prints only the disagreements:

    uv run --no-sync python tools/probes/pg_differential.py \
        tools/probes/pg_corpora/windows.setup.sql \
        tools/probes/pg_corpora/windows.sql --types

Both servers are driven through the **same `psycopg` connection**, so
client-side type mapping is identical and every difference is the server's.
Two rules that the 2026-09-03 sweeps paid for:

- **Do not probe through `run_sql()`.** The embedded API returns the engine's
  internal values, which the wire converts on the way out — a `numeric(p,s)`
  cast surfaces as a raw `Decimal128`, a `::date` as a `str`, an interval as a
  subdocument. Probed directly all three look like divergences; over the wire
  all three are correct. That was 4 of the first 8 "findings".
- **Pass `--types`.** A wrong oid under a right value is invisible to a row
  comparison, and several real bugs were exactly that: `#>` sent jsonb under
  oid 25, `LIKE ... ESCAPE` sent a boolean as `'t'`, a window `sum(int4)`
  declared int4 where PG promotes to int8.

**Check `SHOW lc_ctype` before believing a case-mapping difference.** This box's
PostgreSQL runs the `C` locale and does not case-map non-ASCII at all
(`upper('é')` is `é`), so several apparent `initcap` / `upper` bugs are locale
data rather than engine behaviour.

## Why these live in the repo

The differential tooling that found nine bugs during the 2026-08 audit "lived in
a scratchpad and died with the session" (see that test file's docstring). These
found another 113 findings across two sessions' worth of areas and were heading
for the same fate. Keeping them costs nothing and means the next person starts
from a corpus rather than from scratch.

## Running

Start a mongod, then point a probe at it:

    mongod --dbpath /tmp/probe --port 27041 --setParameter enableTestCommands=1 &
    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run --no-sync python tools/probes/arg_types_documents.py

**Sweeping the RUST server.** `arg_types_extended.py` takes `PROBE_SERVER`, a
URI of an already-running server, instead of starting an embedded Python one.
The probes drive the wire, so the server under test is just an address:

    ./inv rust-binary-build
    crates/secantusdb/target/debug/secantusd-rs --port 27055 --storage-path /tmp/rs &
    PROBE_SERVER="mongodb://127.0.0.1:27055" \
      PROBE_MONGOD="mongodb://127.0.0.1:27041" \
      uv run --no-sync python tools/probes/arg_types_extended.py

The first such sweep (2026-08-29) found the Rust server at **78 of 87
divergent** while the Python server was 87/87 clean — the two servers are NOT
interchangeable for conformance, and a probe that has only ever run against one
of them says nothing about the other.

**Three mongod versions are installed here.** `mongod` on `PATH` is **8.2.11**
(`mongodb-community@8.2`, linked as default so the differential gate runs rather
than skipping); 6.0.16 is at `/opt/homebrew/opt/mongodb-community@6.0/bin/mongod`
and 8.3.4 at `.../mongodb-community@8.3/bin/mongod`. Probe more than one whenever
a result could be version-shaped.

The change-stream sweep is the cautionary case. Probed on 8.3.4 it looked like
`fullDocument` had moved position in "8.x", and a backlog entry was written
claiming a retarget gap. On 8.2.1 — the version actually targeted — it had not
moved at all: 6.0.16 and 8.2.1 agree and only 8.3 differs. A major series is not
one behaviour, and neither is a minor one: 8.2.1 and 8.2.11 disagree on the order
of a wrong-type error's type list. (8.x dropped `--fork` on macOS; background it
with `nohup … &`.)

**Probe the mongod version SecantusDB targets.** We advertise 7.0
(`buildInfo.version`, maxWireVersion 17) and `tests/test_mongod_differential.py`
spawns whatever `mongod` is on PATH. Probing a newer server than that and
"fixing" the difference is a real trap: it shipped a message-prefix change that
broke five live differential cases, because 8.3 wraps update errors in
`Plan executor error during update :: caused by ::` and 6.0 does not. Probe the
PATH version first, then cross-check a newer one to learn which differences are
version-dependent.

**Change streams need a REPLICA SET.** mongod refuses `$changeStream` on a
standalone, and `tests/test_mongod_differential.py`'s harness spawns a
standalone — which is why that whole area had never been differentially probed
until `change_streams.py`. Start one explicitly:

    mongod --replSet rs0 --port 27045 --dbpath /tmp/csrs --fork --logpath /tmp/csrs/log
    mongosh --port 27045 --eval 'rs.initiate()'

## The probes

| script | area | last result |
|---|---|---|
| `arg_types_documents.py` | document-valued command arguments | **0 of 56 on both servers** (2026-09-02). The Rust column was added that day and immediately found **12**: a non-document `q`, or a non-document/array `u`, fell through every match arm so the statement applied nothing and answered `ok`. Was 45 crashes on the Python side originally |
| `arg_types_extended.py` | more commands + numeric/string/bool argument classes | **244/244 clean on both servers** against mongod 8.2.11 (2026-08-31). Compares CODES only — see `arg_types_messages.py` below |
| `arg_types_messages.py` | the same class, comparing MESSAGES, over 685 shapes and ~80 more slots | **0 of 685 on both servers** (Rust was 550 code + 50 message divergences over 76 slots; the Python half closed in #1152) |
| `change_streams.py` | change events, event field order, fatal errors | **0 of 41, and 0 field-order differences**, on both servers against mongod 8.2.11 (2026-08-30) — the sweep is closed. Was 14/41 when first run. **Needs a replica set**, see the script |
| `collation_order.py` | collated ORDER, with and without a collated index | **17 of 19 exact** against mongod 8.2.11 (2026-09-01); the 2 remaining are the LOCALE gap (Swedish `ä` after `z`), which needs CLDR data. Was 0 of 19 -- ordering had never been implemented, only matching |
| `explain_shapes.py` | `explain`'s normalised `parsedQuery` and its stage tree | **`parsedQuery` 0 of 56; `winningPlan` 18 of 25** against 8.2.11 (2026-09-01). `PROBE_ORDER=1` re-derives the `$and` child ordering pairwise -- it is mongod's internal match-type ordinal and is documented nowhere |
| `index_result_sets.py` | does an INDEX change which documents come back | **0 of 1803 on BOTH servers** (2026-09-03), after the corpus grew NaN, the infinities, `Decimal128`, `MinKey`/`MaxKey`, `ObjectId`, a date and an `Int64` — which immediately found a **partial index dropping every NaN row on both servers**. Compares the server against ITSELF, with and without the index, plus the `_id` SET against mongod. The only probe here that is not primarily a mongod comparison, because 'the index dropped rows' is invisible to one: the query succeeds and the shape is right. `PROBE_SEED` / `PROBE_TRIALS` re-run it at a different seed — 3 extra seeds x 120 trials were also clean |
| `findandmodify_shapes.py` | findAndModify replies and argument validation | **0 of 18 on both servers** (2026-09-02). The Rust column found **6**: `remove` alongside `new` or `upsert` RAN THE DELETE where mongod refuses, and a remove's `lastErrorObject` carried `updatedExisting`, which mongod reports only for an update |
| `update_operators.py` | update operator semantics and errors | 70 shapes: **python 0, rust 7** (2026-09-03), after 34 shapes covering NaN / infinities / `Decimal128` / int64 overflow / `MinKey`-`MaxKey` were added to the original 36. That widening found `$min`/`$max` ranking NaN by IEEE rules rather than sort order on both servers. Of the seven Rust ones, two are the missing `Plan executor error during update :: caused by ::` wrapper, three are int64-overflow messages, and two are a `modifiedCount` difference. Filed in `tasks/backlog.md` |
| `update_path_conflicts.py` | overlapping update operator paths | **0 of 11 on both servers** (2026-09-02) — clean on the Rust column from the first run, the only one of the five that was. Was 8 wrong results on the Python side originally |
| `operator_error_surface.py` | every query/update operator crossed with every pathological argument | **0 of 3074 on both servers** (2026-09-01). Was 583 on the Python side and 1053 on the Rust one; 999 of the Rust figure answered a generic BadValue for arguments mongod names precisely |
| `aggregation_stage_specs.py` | malformed aggregation STAGE specs | **0 of 740 on both servers** (2026-09-02). Python was 167 then 22; the Rust column, added that day, was **219** — including `$out`/`$merge` with an empty target namespace returning `ok` having written nothing. The `NEARLY_VALID` cases exist because the scalar corpus could not reach the checks that run after a stage's leading required field is satisfied |
| `date_timezones.py` | the date family crossed with timezones, units and bin sizes | **0 of 409 on both servers** (2026-09-02). Was python 142 / rust 191. `$dateTrunc`, `$dateDiff` and `$dateAdd` each ignored `timezone` outright — silent wrong answers on both servers, not errors |
| `regex_value_semantics.py` | a regex as a VALUE, not only a pattern | **0 of 104 on both servers** (2026-09-02), plus an indexed-vs-unindexed sort check. Was python 14 / rust 21: a bare `/ab/i` never matched a stored regex equal to it |
| `addtoset_membership.py` | `$addToSet` membership equality, and the `$pop`/`$max`/`$min` arguments | **0 of 30 on both servers** (2026-09-02). Was python 1 / rust 5 |
| `range_type_brackets.py` | range operators inside one BSON type bracket | **0 of 112 on both servers** (2026-09-01) |
| `agg_expressions.py` | every aggregation EXPRESSION operator (143), 1- and 2-argument forms | **6628 cases** (was 3968 until the corpus was widened on 2026-09-03). **python: 0 crashes, 24 wrong values, 31 code / 173 message**; **rust: 0 wrong values, 91 code / 17 message**. The widening — infinities, NaN, signed zero, the int32/int64 boundaries, MinKey/MaxKey/Binary/Timestamp/Regex/Code, empty and nested containers — immediately found **13 crash-class bugs** and 6 wrong values on surfaces this corpus had swept tens of thousands of times. A value CLASS that is absent is invisible exactly the way a passing test is. See `tasks/backlog.md` §7 for what remains |

## `_servers.py`

`probe_targets()` gives a probe both servers plus mongod, in one place. Five
probes here compared only the PYTHON server until 2026-09-02, which is how the
Rust server came to be 219 divergent shapes on the stage corpus, 12 on the
write-argument one and 6 on findAndModify with nobody the wiser — and how a
`{$setEquals: []}` panic went unseen. A probe without the Rust column proves
half of what it claims. It prints a loud note when the extension is not built,
so a clean run is never mistaken for a compared one.

## Writing a new one

Keep each case tiny and independent: seed a document, run one operation, compare.
A case that disagrees then names one behaviour, so the failure is actionable.
Print only divergences — a probe that prints its passes buries its findings.
**Print all of them, too:** two of these probes capped the printed list (`diffs[:22]`,
`diffs[:14]`) while reporting an accurate total, so a reader who worked from the
list silently missed findings — it happened, and cost a backlog entry that named
two thirds of the divergences it claimed to enumerate.
