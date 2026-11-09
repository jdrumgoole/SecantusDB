### Every write's interrupt-check comparison allows for mongod's write-concern races

Every wait that a write's write concern makes inside mongod is a race: the oplog's holes, the `j` journal flush and the `majority` commit point. A wait whose future had already finished makes no checks. One that finishes while it is being waited on makes one check instead of two. Linux mongod 8.2.12 in CI showed this twice in a day: two checks short for a `j` update, and one short for an `insert of several` under the default write concern. macOS mongod, whose journal sync is slower, waited every time in 25 runs of each write. This server always counts the waited branch. Every write scenario in `tests/mongod_live/test_interrupt_checks.py` now looks for this server's count among up to five of mongod's runs, where before only the two `j` scenarios did.

#### Fixed

- `tests/mongod_live/test_interrupt_checks.py::test_a_write_line_counts_mongods_interrupt_checks`: no longer fails when Linux mongod's write-concern wait finds its condition already met (`[insert of several]`, PG snapshot #2574).
