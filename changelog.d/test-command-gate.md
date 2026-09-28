### `configureFailPoint` is now gated behind `enableTestCommands`

W6 in `docs/security-reports/2026-08-10.md`: an unauthenticated client that can
reach the port could arm a server-wide `failCommand` with
`closeConnection: true`, dropping the socket of every subsequent operation on
*every* connection — a cross-tenant DoS with no privilege required. mongod ships
the same command behind a startup parameter, off by default. SecantusDB had no
equivalent gate; the feature was always live.

Measured against mongod 8.2.11 (2026-09-28): started without the parameter it
answers `59 CommandNotFound :: no such command: 'configureFailPoint'` — not
`Unauthorized`, not a no-op — and `getParameter` reports
`enableTestCommands: false`. Both servers now answer identically.

#### Changed

- **The standalone daemons default it OFF**, as mongod does:
  `--enable-test-commands` on `secantusd-rs` and `secantusd-py`, or
  `[server] enable_test_commands` in `secantusd.toml`.
- **The embedded `SecantusDBServer` defaults it ON.** Constructing one in a test
  is the entire use case for that class, and every driver failpoint suite needs
  it. The daemon is the thing an operator exposes on a port; the embedded handle
  is not.
- `getParameter` reports the **real** value instead of a hardcoded `true`.
  Drivers gate their failpoint suites on this flag — while it merely *said*
  false, pymongo skipped ~1,080 unified-spec tests — so a server that refuses
  the command must not claim to accept it.

#### The gate is the registry's absence

A server that did not enable test commands wires no `FailPointRegistry`, and
the missing registry is what makes the command report `CommandNotFound`. That
keeps the decision at server startup, where the operator made it, instead of
threading a flag down to the dispatch table.

#### One choke point for the gauges, not thirteen

`tasks/driver-conformance-followups-plan.md` sized the cost as *"every gauge
task must pass the flag. Miss one and that gauge silently loses its failpoint
coverage."* `gauge_common.spawn_daemon` already rewrites the port and log level,
so it forces the flag too — all thirteen daemon gauges get it from one place,
and the failure mode the plan predicted cannot happen.
