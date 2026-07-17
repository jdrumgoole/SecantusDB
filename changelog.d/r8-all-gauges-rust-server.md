### The Rust server clears all thirteen driver-conformance gauges

Every one of SecantusDB's thirteen driver-conformance gauges now runs against
the Rust server, not just pymongo — pymongo (sync + async), the mongo-go /
node / java / kotlin / ruby / rust / php-library / php-driver / c / c++ / .NET
drivers — and the Rust server reaches effective conformance parity with the
mature Python server across every ecosystem. Three gauges pass perfectly
(mongo-rust-driver, .NET, kotlin at 100%), nothing scores below 98%, and no
failure is a new Rust-specific divergence: each one traces to a gap that is
already out of scope for a single-node surrogate (text / hashed indexes,
`$where`, multi-node transactions and sessions, Atlas search-index
management, IPv6) or to a documented driver-side or test-harness artifact the
Python server exhibits too. The per-gauge scoreboard and the follow-up triage
notes (a handful of assertion failures to diff against the Python-server runs)
live in `tasks/backlog.md` under the R8 entry; each run's full report is
committed as `docs/validation-report-<driver>-rust-server.md`.

#### Added

- Committed Rust-server conformance reports for all thirteen gauges under
  `docs/`, and a full-sweep scoreboard in the backlog's R8 entry.
