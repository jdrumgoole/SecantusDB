### Test temp dirs are reclaimed on the way out, and probe stores are reclaimed at all

A dev box filled its disk — 935 GiB, down to 50 MB free — with every janitor in
the tree working exactly as designed. Three things were wrong at once, and none
of them was the reaping *rule*.

The retained-run count was set against a suite that no longer exists: the code
said "~1.7 GiB a run", a measured full run leaves **~104 GiB**, and keeping
three of those is ~300 GiB of WiredTiger homes. The sweep also ran only at
session *start*, so whatever the last run left stayed on disk until somebody ran
pytest again — which may be days, or may be impossible because the disk is
already full. And the ~17 differential probes under `tools/probes/` each took a
bare `tempfile.mkdtemp()` that nothing ever deleted: invisible to pytest's
janitor, which only manages `pytest-of-<user>/`, and to the gauge sweep, which
only matches `secantus-*-gauge-*`. One session left 385 of them, ~50 GiB.

#### Fixed

- Keep **one** abandoned pytest run instead of three, and re-derive the
  per-run cost the comment quotes. The newest run is what a post-mortem needs;
  the two behind it were 200 GiB nobody read.
- Reap at session **finish** as well as session start, so the common case — run
  the suite, walk away — leaves the disk as the retention policy intends. The
  current run is protected by both existing rules (newest dir, live PID in its
  `.lock`).
- Probe stores are deleted when the probe exits, and carry their creating PID
  so an abandoned one can be reaped later. The `atexit` delete cannot be the
  whole answer — on Windows an open file cannot be deleted, so a probe killed
  mid-flight leaves its home behind with WiredTiger still holding it — hence
  `_sweep_stale_probe_tmp`, which applies the same PID-liveness rule the pytest
  sweep already used, and now runs from both `invoke clean` and pytest.

#### Added

- `tools/probes/_servers.probe_store()`: one self-cleaning, PID-tagged
  WiredTiger home for a probe, replacing the bare `mkdtemp()` in every probe.
- A test pinning that `conftest.py` defines `pytest_sessionfinish` and
  `pytest_sessionstart` exactly once each. The reap was first written as a
  second `pytest_sessionfinish`; Python silently kept only the later
  definition, so it never ran and nothing failed to say so.
