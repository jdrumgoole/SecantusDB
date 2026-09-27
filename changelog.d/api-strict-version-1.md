### `apiStrict: true` now rejects a command outside the Stable API

A client that declares `serverApi: {version: "1", strict: true}` is asking the
server to refuse anything outside MongoDB's Stable API Version 1. SecantusDB
accepted those commands and ran them, which is the one thing a strict client has
explicitly asked you not to do — a driver using strict mode to catch
non-portable calls in CI got a clean run and a false sense of portability.

Both servers now answer mongod's `APIStrictError` (323), with mongod's own
message, for a command outside Version 1. The rule has three branches rather
than the two the specification's wording suggests, and the third is the one that
matters: a command the server *has* but that sits outside Version 1 is rejected,
a command inside Version 1 runs, and a command that **does not exist at all**
still answers `CommandNotFound` (59) even under `apiStrict`. A check that simply
refused everything it did not recognise would get that last case wrong.

The membership list was measured, not transcribed, by sending every command
SecantusDB implements to a real mongod 8.2.11 under `apiStrict: true` and
recording which it refused. That is how `distinct`, `buildInfo`, `isMaster` and
`serverStatus` turned out to be *outside* the Stable API while `count` and
`hello` are inside it — a set assembled by reading the manual would have had
several of those the wrong way round.

#### Fixed
- `commands.py` / `crates/secantus-commands`: `apiStrict: true` answers
  `APIStrictError` (323) for a command outside Stable API Version 1, with
  mongod's verbatim message including the dochub link.
- The aggregation-stage message was wrong on both servers. mongod says
  `$listLocalSessions is not allowed with 'apiStrict: true' in API Version 1`;
  we said `Provided aggregation pipeline stage ... is not in API Version 1`.
- Closes the `apiStrict` failure in **two** driver gauges — pymongo's
  `TestVersionedApiTestCommandsStrictMode` and the Java driver's
  `VersionedApiTest` strict-mode case. Two unrelated drivers failing the same
  behaviour is what marked it as a real gap rather than a harness artifact.

#### Changed
- Replaced the narrow `_API_V1_REJECTED_BY_NAME` gate (just `distinct`, with a
  non-mongod message) and the Rust `name == "distinct"` canary with one measured
  allowlist per server. The old gate's comment said a full allowlist "would
  reject `count`"; probing mongod showed `count` is in Version 1, so that
  objection was a guess rather than a measurement.

#### Testing
- `tests/test_mongod_differential.py`: eleven cases asserting an exact match —
  code, codeName and errmsg — against a live mongod, covering all three
  branches. `testVersion2` is deliberately excluded: it is gated behind
  `enableTestCommands`, so including it would assert the fixture's configuration
  rather than our conformance. The driver gauges cover that one.
