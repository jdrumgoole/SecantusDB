### The test suite reclaims its own disk

Every test in this suite builds a real WiredTiger database — never a mock — so
every run leaves a few gigabytes of per-test stores behind. pytest normally
sweeps old runs itself, but it stops the moment a run dies without running its
exit hooks: the dead run's lock file makes pytest treat the directory as live
for three days. The backlog then compounds, because the bigger it gets the
longer each run's exit-time cleanup takes, so more runs get killed mid-cleanup,
each leaving another stale lock. One machine reached 241 directories and
391 GiB that way; another reached 48 GiB in a day.

`invoke clean` has been able to fix this for a while. The problem was that it
only ran when somebody remembered, so the backlog kept coming back. The sweep
now also runs automatically when a pytest session starts.

It deletes only directories whose owning pytest process is **gone**, decided
from the PID in the lock file, and never the newest few. A live run is
protected twice over: its directory is the newest, and its lock names a running
process.

#### Added

- `tests/conftest.py` reaps abandoned pytest temp trees at session start. It
  runs on the xdist controller only, skips the byte-counting walk that
  `invoke clean` does for its summary line, and swallows every error —
  reclaiming disk must never fail a test run. Set `SECANTUS_NO_TMP_REAP=1` to
  turn it off.
- `_sweep_stale_pytest_tmp` takes `measure=False` for callers that want the
  deletion without sizing every tree first.

#### Note

Deleting a *passed test's* own `tmp_path` mid-session is a different thing and
remains forbidden — it races WiredTiger's background threads into `WT_PANIC`,
and `tests/conftest.py` refuses to start under a retention policy that does it.
This sweep only ever touches trees from runs that have already exited.
