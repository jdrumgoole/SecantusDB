### The differential that found nine bugs is now part of the suite

The 2026-08 backlog audit found nine real defects — three that surfaced an
unhandled exception as "internal server error", three that silently wrote or
returned wrong data, one where adding an index changed query results, and two
missing capabilities. Every one came from running the same operation against
SecantusDB and a real mongod and comparing. None came from reading the backlog,
whose entries for those areas were absent, stale, or wrong.

That comparison lived in a scratchpad script. It is now `tests/test_mongod_
differential.py`: small independent cases, mongod as the oracle, errors compared
as values because a wrong error code is a real divergence too.

It skips when no `mongod` is on PATH — the same convention the mongosh and
database-tools tests already use — so it is free on machines without MongoDB
installed and gives real coverage where it exists.

#### Added

- `tests/test_mongod_differential.py`, 16 cases covering every bug the audit
  found, plus a `differential` pytest marker (`pytest -m differential`).
