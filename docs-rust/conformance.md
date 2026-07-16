# Conformance

The Rust server is measured the same way the Python server is: **unmodified
upstream driver test suites** run against it, exactly as the driver
maintainers run them against a real `mongod`.

- **pymongo**: 99.5% of pymongo's own suite (1020 of 1025 running tests) —
  level with the Python server. The
  [Rust-server validation report](https://secantusdb.com/docs/validation-report-rust-server.html)
  regenerates weekly with the exact failing-test list.
- **mongo-java-driver**: 99.6% of the gauged suite; the two failures are
  the `mapReduce` tests (the Rust server does not implement `mapReduce`).
  See the
  [Java Rust-server report](https://secantusdb.com/docs/validation-report-java-rust-server.html).
- The C, C++, C#, Kotlin, Node, and Rust driver gauges were audited against
  the Rust server with **zero Rust-only failures** — every remaining skip
  is a shared out-of-scope feature, not a Rust-server gap.

The
[feature comparison](https://secantusdb.com/docs/feature-comparison.html)
maps both servers against real MongoDB feature by feature — commands,
query/update/expression operators, aggregation stages, indexes, change
streams, transactions, auth, and backup/PITR. The short version of what the
Rust server does **not** have relative to the Python server:

- the SQL / PostgreSQL wire frontend (Python-server-only),
- `mapReduce` and `top` (answered with `CommandNotFound`),
- `secantusAdmin.restoreToTimestamp` over the wire (use
  [`secantusd-rs restore`](recovery.md) instead),
- session-lifecycle commands beyond `startSession` are acknowledged no-ops,
- a handful of operator edge cases the Rust engine deliberately rejects
  rather than risk diverging from the Python oracle (some
  `$dateFromString` / `$dateToString` format directives, Decimal128
  arithmetic edges, mixed-type sort orderings).

Everything else — CRUD, aggregation, change streams with pre-images and
resume, multi-document transactions, indexes (compound, partial, TTL,
geo), auth/RBAC/TLS, backup/PITR — is at parity.
