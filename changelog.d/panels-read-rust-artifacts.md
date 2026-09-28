### The published driver panels could only ever show the Python server

Every gauge has written a `-rust-server` artifact for months, and nothing read
them: all thirteen collectors hardcoded the Python filenames. The page's own
prose carried a note saying the Rust numbers were not shown — a documented
limitation rather than a fixed one.

#### Added

- `--server python|rust` on `validation_summary.driver_panels`. The Rust grid
  now renders from the Rust artifacts.
- **A mixed-age guard.** The grid is published as ONE snapshot with one implied
  date, so a panel built from a months-old artifact reads as current to everyone
  who sees it. There was no check at all: the first Rust render would have put a
  **10 August** pymongo-async rate beside twelve numbers measured that morning.

  It tests SPREAD rather than absolute age — a deliberately old but consistent
  sweep is honest, and it is the mixing that misleads. `--allow-stale` overrides
  it for the rare case where a mixed grid is genuinely wanted.

- `GAUGE_ARTIFACTS`, one map from panel name to artifact filename, so the
  freshness check reaches the same files the collectors do. A test pins it
  against the collector registry: a gauge added to one and not the other would
  be silently exempt from the check, which is the exact failure the guard exists
  to prevent.
