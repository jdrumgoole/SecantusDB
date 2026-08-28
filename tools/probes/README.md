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

**Probe the mongod version SecantusDB targets.** We advertise 7.0
(`buildInfo.version`, maxWireVersion 17) and `tests/test_mongod_differential.py`
spawns whatever `mongod` is on PATH. Probing a newer server than that and
"fixing" the difference is a real trap: it shipped a message-prefix change that
broke five live differential cases, because 8.3 wraps update errors in
`Plan executor error during update :: caused by ::` and 6.0 does not. Probe the
PATH version first, then cross-check a newer one to learn which differences are
version-dependent.

## The probes

| script | area | last result |
|---|---|---|
| `arg_types_documents.py` | document-valued command arguments | 56/56 clean (was 45 crashes) |
| `arg_types_extended.py` | more commands + numeric/string/bool argument classes | **0 crashes, 42 divergences — open** (crashes closed by #1080; re-measured 2026-08-28 vs mongod 6.0.16) |
| `findandmodify_shapes.py` | findAndModify replies and argument validation | 18/18 clean (was 6 divergences) |
| `update_operators.py` | update operator semantics and errors | clean except the filed items |
| `update_path_conflicts.py` | overlapping update operator paths | 12/12 clean (was 8 wrong results) |

## Writing a new one

Keep each case tiny and independent: seed a document, run one operation, compare.
A case that disagrees then names one behaviour, so the failure is actionable.
Print only divergences — a probe that prints its passes buries its findings.
**Print all of them, too:** two of these probes capped the printed list (`diffs[:22]`,
`diffs[:14]`) while reporting an accurate total, so a reader who worked from the
list silently missed findings — it happened, and cost a backlog entry that named
two thirds of the divergences it claimed to enumerate.
