### Rust MongoDB server: handshake, diagnostic and session commands checked against mongod

The commands a driver or a tool sends about the server and the connection
(`hello`, `buildInfo`, `serverStatus`, `getParameter`, `dataSize`,
`$collStats`, the session commands and the fields every command takes)
were sent to the Rust MongoDB server and to mongod 8.2.11, 1,141 of them,
and each reply compared. 492 differed; @@LEFT@@ do now
(`tasks/backlog.md` 7.082).

#### Fixed

- `dataSize` with `min` and `max` sizes that key range. It sized the whole
  collection for any range.
- The `storageStats` of a `$collStats` stage are the collection's sizes.
  They were 0.
- `serverStatus` reports the process id, the uptime, the operations run
  (`opcounters`) and the bytes read and written. Each was a constant 0.
- A wrong-typed `$readPreference`, `$clusterTime`, `apiVersion`,
  `apiStrict`, `readConcern.afterClusterTime` or `atClusterTime` is
  refused. Each was read as absent. A read at an `atClusterTime` more than
  five minutes old is `SnapshotTooOld`; it was answered with the current
  documents.
- A command given a field it does not have is refused where mongod refuses
  it (`{ping: 1, bogus: 1}`).
- `hello` answers `isWritablePrimary` and the two older names answer
  `ismaster`, in mongod's key order, with an int32 `connectionId`. The
  `client` metadata is checked, and refused on a second `hello`.
- Which commands are in API Version 1, and which run only against `admin`,
  comes from mongod's own command table. `collStats`, `createUser` and
  eight more were taken under `apiStrict: true`.
- `whatsmyuri` gives the client's address. `killOp` takes any whole number
  and does not say what it found. `getParameter` refuses a request that
  names no parameter it has, and answers `showDetails`.
- `profile`, `validate`, `endSessions`, `killSessions`,
  `killAllSessions`, `usersInfo`, `rolesInfo`, `saslStart`,
  `saslContinue`, `$collStats`, `$indexStats`, `$currentOp`,
  `$listSessions` and `{aggregate: 1}` check their arguments.
- `$listSessions` and `$listLocalSessions` list the sessions commands have
  come in. They answered a row describing an operation.

#### Added

- `listCommands`, `logout`, `getDefaultRWConcern`, `replSetGetConfig`,
  `shardingState`, `fsyncUnlock`, and the lower-case `collstats` and
  `datasize`.
- `tools/probes/diagnostic_commands.py`.

#### Changed

- A `filter` document on `profile` is refused. It was taken and ignored.
- `killOp` no longer takes a string of digits, and `writeConcern` no
  longer takes `wtimeoutMS`: mongod takes neither.
- `getParameter` no longer reports `version`, which is not a parameter.
