### The benchmark page now covers the paths that differentiate — and the PGO profile catches up

The published nine-workload latency table gains two rows the old six-row
table never measured: a **filtered collection scan** (the per-document
compare path — the one the new allocation-free numeric fast path
accelerates; the unfiltered scan and the indexed range never touch it) and
a **change-stream drain**, where the Rust server now clocks **0.8× of
mongod — faster than mongod at its own change streams** — after the reply
path stopped re-encoding event blobs. The aggregate multi-stage workload
joins the published table too. The committed PGO profile is regenerated on
the post-review hot paths (a stale profile silently forfeits its 12–19%),
and every surface that quotes the ×mongod ranges — the benchmark page, the
website performance page, the Rust-server docs, the README — is re-baselined
from the same fresh five-rep run.

#### Changed
- `bench/compare_servers.py`: new `find_filtered_scan` and
  `change_stream_drain` workloads; the change-stream reference spawns a
  single-node replica-set mongod (its change streams require one) while
  every other row keeps the standalone reference; the Rust server arm
  advertises the replica-set persona to match the Python server.
- `crates/pgo/_secantus_server.profdata.tar.gz`: retrained via
  `invoke rust-pgo-refresh` on the post-micro-opt hot paths.
- `docs/benchmark.md`, `docs-rust/index.md`, `README.md`, website
  performance page: nine-row table + refreshed charts and ×mongod ranges
  (Rust ~0.8×–2.3×; three rows beat mongod outright).
