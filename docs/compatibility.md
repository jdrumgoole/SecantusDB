# Compatibility

SecantusDB's conformance target is **`pymongo`**: a `pymongo` client should
not be able to tell SecantusDB apart from a real `mongod` for the operations
it supports. This page lists the divergences that exist anyway.

## Stubs

These commands accept the request and return a wire-valid response, but the
response is fabricated.

| Command | Behaviour |
| --- | --- |
| `serverStatus` | Version + zeroed metrics (uptime, connections) |
| `connectionStatus` | Real `authenticatedUsers` (from SCRAM-SHA-256). `authenticatedUserRoles` / `authenticatedUserPrivileges` are empty (RBAC not enforced — see [Authentication](authentication.md)) |
| `hostInfo` / `whatsmyuri` / `buildInfo` | Hardcoded values; `buildInfo.version` is `"7.0.0"` |
| `getLog` | Empty log array |
| `startSession` / `endSessions` / `refreshSessions` | `startSession` returns a fresh UUID; the others are no-ops. **No session state is tracked**, so cross-session correlation isn't enforced |
| `abortTransaction` / `commitTransaction` | Return `{ok: 1}` but **do not roll back**. Operations inside a transaction take effect immediately. Tests that depend on real transactional rollback need a real `mongod` |

`dbStats` and `collStats` return real `count`, `size`, `storageSize`,
`avgObjSize`, `indexSize`, `indexSizes`, `totalIndexSize`, and `totalSize`
computed from the WT tables.

`explain` reports `IXSCAN` when an index would be used and `COLLSCAN`
otherwise, with `indexName`, `keyPattern`, and `direction` populated.

## Stopgaps (functional but with limitations)

### `$lookup` doesn't use storage indexes

Joins are O(N+M) via an in-memory hash table built once over the foreign
collection (covers array-valued local/foreign fields correctly via
element expansion). Both simple (`localField`/`foreignField`) and
`let`/`pipeline` forms are accelerated.

A true index-driven join would skip materialising the foreign collection
but needs multikey-index support to stay correct for array-valued foreign
fields.

### `_id` numeric type bridge

Works for finite int / float / Decimal128 (they collide on equal value).
`bool` is deliberately not numeric. NaN and infinity `_id` values fall
through to the BSON-blob path; behaviour is unspecified.

### Date format strings

`$dateFromString` and `$dateToString` use Python's `strptime` / `strftime`
codes plus the `%L` extension for milliseconds. The `timezone` argument
is supported (IANA, UTC offsets, `GMT`/`UTC`).

Still missing: full MongoDB format spec (`%G` / `%V` ISO-week, `%j`
day-of-year edge cases) and the MongoDB-specific format tokens.

### `$merge` `whenMatched: "merge"`

Recursive sub-document merge implemented (matches MongoDB). Arrays are
replaced as a whole on overlapping keys.

### `renameCollection`

Atomic per the storage `RLock`, but no protection against concurrent
writers across processes. Tests are single-process so this is fine.

### `createIndexes` options

| Option | Status |
| --- | --- |
| `unique` | Honoured |
| `sparse` | Honoured |
| `expireAfterSeconds` | Honoured via `Storage.prune_ttl` (opt-in; no background sweeper) |
| `partialFilterExpression` | Honoured at write time and at picker time |
| `collation` | Accepted but ignored (Python compares with default locale) |

### Cursor TTL

Cursors idle longer than 600s are pruned opportunistically (matches
MongoDB's 10-minute cursor TTL). The clock is injectable via
`time_func` for deterministic tests.

## Deferred

- Aggregation expressions: `$function` (out of scope — needs JS).
- Aggregation stages: `$fill` (planned), `$densify` with `unit:` for
  date ranges (planned; numeric `$densify` is implemented).
- `mapReduce` — deprecated by MongoDB; not implemented.

## Out of scope

These are explicit non-goals:

- **Replica sets / sharding** — depend on multi-node cluster topology.
  SecantusDB is single-process. (Change streams *are* supported —
  oplog-backed and single-node — see [Change streams](change-streams.md).)
- **Authentication mechanisms beyond SCRAM-SHA-256** — x509, LDAP,
  Kerberos, GSSAPI, MONGODB-AWS, MONGODB-OIDC. SCRAM-SHA-256 itself
  *is* implemented; SCRAM-SHA-1 is not advertised (modern drivers
  default to SHA-256). See [Authentication](authentication.md).
- **Authorization (RBAC)** — `createUser` accepts a `roles` array but
  no command consults it. An authenticated principal is treated as
  fully privileged.
- **TLS / SSL.** SCRAM credentials therefore travel in plaintext —
  use only on a trusted network.
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told
  the server doesn't support compression.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes).
- **Geo** (`$near`, `$nearSphere`, `$geoWithin`, `$geoIntersects`,
  `2d` / `2dsphere` indexes).
- **`$where`** — runs JavaScript. We don't ship a JS runtime.
- **Capped collections** — fixed-size, FIFO collections.
- **Profiling** (`setProfilingLevel`, `system.profile` collection).
- **Tailable cursors on capped collections** — capped collections are
  out of scope, so tailable-on-capped is too. (The change-stream
  cursors *are* tailable with `awaitData` blocking against the oplog —
  see [Change streams](change-streams.md).)

## Known edge cases

- **`$sample`** uses `random.sample` without a fixed seed. Deterministic
  only if the test does `random.seed(...)` first.
- **`$type: "number"`** in queries handles `int`, `float`, `Decimal128`,
  but the int32-vs-int64 distinction depends on the Python value range,
  not the original BSON type tag (which is dropped on decode). A doc
  inserted as `Int64(5)` reads back as a small Python int and matches
  `$type: "int"`, not `"long"`.
- **`$lookup` simple-form-plus-pipeline** — when both `localField` /
  `foreignField` and `pipeline` are present, we pre-filter by the simple
  form and then run the pipeline. Real MongoDB does this in modern
  versions; documentation isn't crystal clear on the order. If a test
  breaks here, this is the place to look.
- **Aggregation `$group` stable order** — group buckets are emitted in
  first-seen order, not sorted. Matches unsharded MongoDB; sharded
  behaviour isn't modelled.
