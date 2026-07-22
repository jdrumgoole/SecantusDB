### Rust server: disable WiredTiger log pre-allocation

The Rust storage engine opened WiredTiger with log pre-allocation left at the
library default, so every on-disk instance kept two 128 MB `WiredTigerPreplog`
files ready ahead of the active journal — roughly a quarter-gigabyte of idle
disk per instance regardless of how little the database held. Test runs that
spin up thousands of short-lived storage instances accumulated this fast enough
to exhaust the small CI runner disks, surfacing as `No space left on device`
failures in the Windows storage-engine job.

Pre-allocation is a write-latency optimisation for sustained-throughput servers;
SecantusDB's instances are small and short-lived, so it bought nothing and cost
a lot. The Rust engine now sets `prealloc=false` — matching the Python server,
which has always shipped it — dropping each instance's idle journal footprint to
what it actually writes, with no durability change (recovery still replays the
same log records). The 128 MB `file_max` tuning is retained, so a production
sustained-writer still gets full-size active log segments.

#### Fixed

- `secantus-storage` (Rust): WiredTiger connection config now sets
  `log=(prealloc=false)`, eliminating the ~256 MB per-instance pre-allocated
  journal that was exhausting CI runner disks.
