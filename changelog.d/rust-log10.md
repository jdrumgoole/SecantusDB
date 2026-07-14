### `$log10` now evaluates natively on the Rust server

The `$log10` aggregation-expression operator is now computed natively by the Rust
engine, so the Rust server evaluates it instead of rejecting the pipeline with a
`BadValue`. The rest of the transcendental family (`$exp` / `$ln` / `$log`) was
already native; `$log10` had simply been left out. Rust's `f64::log10` and
CPython's `math.log10` share the platform libm, so the two servers agree
bit-for-bit (pinned by the expression parity corpus). Found by a three-way
differential sweep against real mongod 6.0.

#### Fixed

- `$log10` is evaluated by the Rust `secantus-core` expression engine (was a
  Fallback → `BadValue` on the Rust server). Matches the Python server and mongod
  for positive inputs; a non-positive input yields `null` on both servers (see
  `tasks/backlog.md` §7 for the pre-existing log-domain divergence from mongod).
