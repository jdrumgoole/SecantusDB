### The pymongo gauge can now run in parallel

The pymongo conformance gauge has always run serially — one xdist worker
against one embedded server — because the upstream tests share database and
collection names and would trample each other if two ran at once. That is a
property of the shared server, not of the tests: give every worker its own
embedded SecantusDB, with its own WiredTiger store and its own port, and the
collision cannot happen, because no two workers can see each other's
databases.

`invoke validate --jobs N` does exactly that, and distributes whole test files
rather than individual tests, so upstream's within-file ordering — the shared
fixtures and the collections one test creates for the next — is preserved.
Nothing is deselected and nothing is skipped: the same 1,707 tests run, and
measured back to back, every single test's outcome is identical. What changes
is wall time, which drops to roughly the duration of the slowest file — from
155s to 37s on a four-worker run, a little over four times faster.

Some of that is better than linear, because a serial gauge run pays a second
cost that is easy to miss: every file inherits the accumulated state of every
file before it. `test_examples` takes 0.9s against a fresh server and 18s
when it runs late in a serial session; `test_bulk` takes 19s against a fresh
server and 51s in position. Splitting the run across four servers shortens
the history each file sees, on top of running four at a time.

The default stays serial. The published compatibility number is measured the
same way it always has been, so the reports remain comparable release to
release; `--jobs` is for the inner loop, where waiting three minutes to see
whether a fix moved the gauge is the slowest part of the cycle. Four workers
is the practical ceiling — beyond that, CPU contention starts to disturb the
change-stream `awaitData` tests, which measure real elapsed time.

#### Added

- `invoke validate --jobs N` / `invoke validate-pymongo-async --jobs N`: run
  the gauge on N xdist workers, each with its own embedded server
  (`--dist loadfile`, so files stay whole). Default `1` — unchanged serial
  behaviour and an unchanged published number.
- `pymongo_validation/plugin.py`: `SECANTUS_GAUGE_PER_WORKER` makes each xdist
  worker start (and tear down) its own embedded server and overwrite
  `DB_IP` / `DB_PORT` with its own address before pymongo's conftest import.
  The controller still runs the full identity tripwire, so every process
  verifies the server it is about to measure.
