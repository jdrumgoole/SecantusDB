### Failpoints stay with the client that set them, and drivers can now see them

Driver test suites inject errors with `configureFailPoint`, usually scoped to one
client by `appName`, and often on that client's `hello` so its connections drop.
Both servers ignored the scope, so the failpoint reached every connection —
including the test runner's own, which then could not switch it off. One such
test in the Go driver's unified suite left the server unusable for every test
that came after it.

Both servers also told drivers that test commands were disabled, although
`configureFailPoint` works. pymongo reads that flag and skipped every failpoint
test: about 1,080 of its unified-spec tests never ran against SecantusDB.

#### Fixed

- `failpoints.py` / `secantus-commands` `failpoints.rs`: `failCommand` honours
  `data.appName`, matching the connection's client metadata. A handshake `hello`
  is matched by the `client.application.name` it carries itself, so a failpoint
  can fail the first command on a new connection, as the SDAM spec tests
  require. Commands from other clients no longer spend the failpoint's `skip` /
  `times` budget.
- Both servers: `failCommands` compares a command's canonical name, so
  `isMaster` also matches the legacy lower-case `ismaster` handshake that
  pymongo sends (likewise `findAndModify` / `findandmodify`).

- Both servers: a `closeConnection` failpoint now drops the socket on every
  path. The Rust server replied `{ok: 1}` to a legacy `OP_QUERY` handshake,
  which a driver rejects as "wire version 0". Both servers answered a streamed
  (awaitable) heartbeat as a clean end of stream; the Python server also logged
  it as an error. The Rust server's exhaust `getMore` stream ignored it too.

- Rust server: `endSessions`, `killSessions`, `killAllSessions` and
  `killAllSessionsByPattern` abort the sessions' open transactions, as the
  Python server's already did. They were no-ops, so a transaction left open
  kept every later write to its documents in a `WriteConflict` retry loop.
  Driver test runners call `killAllSessions` between tests for exactly this.
  Once the failpoint tests ran, this failed 68 pymongo tests and crashed four
  gauge workers.
- Both servers: a change stream opened or resumed with events already waiting
  no longer returns a `postBatchResumeToken` past events it has not sent.
  - The Rust server took the token from after the whole poll, so when
    `batchSize` held events back for `getMore`, a driver that resumed after the
    first batch skipped them silently.
  - The Python server did the same for `batchSize: 0`.
  - The token is now the last sent event's `_id`, or the resume point when
    nothing was sent.

#### Changed

- Both servers: `getParameter` reports `enableTestCommands: true`. Driver test
  harnesses now run their failpoint tests instead of skipping them. What the
  server accepts is unchanged: `configureFailPoint` was already served, and is
  still RBAC-gated when auth is on.
