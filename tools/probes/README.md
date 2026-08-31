# Differential probes

Small scripts that run the same operations against **SecantusDB and a real
mongod** and print only the disagreements. They are not tests — they are the
tool you reach for when you want to find out what is wrong in an area nobody has
checked, rather than to prove a known thing stays fixed.

`tests/test_mongod_differential.py` is the standing gate; these are the
exploratory front end that feeds it. Anything a probe finds should end up as a
regression test there or in a dedicated test file.

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
| `arg_types_documents.py` | document-valued command arguments | 56/56 clean (was 45 crashes) |
| `arg_types_extended.py` | more commands + numeric/string/bool argument classes | **244/244 clean on both servers** against mongod 8.2.11 (2026-08-31). Compares CODES only — see `arg_types_messages.py` below |
| `arg_types_messages.py` | the same class, comparing MESSAGES, over 685 shapes and ~80 more slots | **0 of 685 on both servers** (Rust was 550 code + 50 message divergences over 76 slots; the Python half closed in #1152) |
| `change_streams.py` | change events, event field order, fatal errors | **0 of 41, and 0 field-order differences**, on both servers against mongod 8.2.11 (2026-08-30) — the sweep is closed. Was 14/41 when first run. **Needs a replica set**, see the script |
| `findandmodify_shapes.py` | findAndModify replies and argument validation | 18/18 clean (was 6 divergences) |
| `update_operators.py` | update operator semantics and errors | clean except the filed items |
| `update_path_conflicts.py` | overlapping update operator paths | 12/12 clean (was 8 wrong results) |
| `agg_expressions.py` | every aggregation EXPRESSION operator (143), 1- and 2-argument forms | first run 2026-08-31: 3884 cases, 57 wrong values, 2288 different codes, 689 message-only, **274 crashes**. After five slices: python **0 crashes**, 682 code / 669 message diffs; rust 1556 code / **0** message. What is left is the per-operator operand-type family — see `tasks/backlog.md` §5 |

## Writing a new one

Keep each case tiny and independent: seed a document, run one operation, compare.
A case that disagrees then names one behaviour, so the failure is actionable.
Print only divergences — a probe that prints its passes buries its findings.
**Print all of them, too:** two of these probes capped the printed list (`diffs[:22]`,
`diffs[:14]`) while reporting an accurate total, so a reader who worked from the
list silently missed findings — it happened, and cost a backlog entry that named
two thirds of the divergences it claimed to enumerate.
