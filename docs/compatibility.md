# Compatibility

SecantusDB's conformance target is **`pymongo`**: a `pymongo` client should
not be able to tell SecantusDB apart from a real `mongod` for the operations
it supports. This page lists the divergences that exist anyway.

:::{note}
This page describes the **Python server** — the conformance reference.
SecantusDB also ships a separate **Rust server** that speaks the same wire
protocol and now matches the Python server on pymongo's suite; its few
remaining feature gaps are delineated in [The two servers](servers.md), and
the full three-way matrix against real `mongod` is in the
[Feature comparison](feature-comparison.md). The separate
**PostgreSQL-wire SQL front end** (`pip install "secantus[sql]"`) has its own
conformance target (`psql` + `psycopg`) and its own supported-SQL matrix in
[SQL / PostgreSQL interface](sql.md).
:::

## Stubs and partial replies

Most diagnostics commands now return real data; the remaining fabrications:

| Command | Behaviour |
| --- | --- |
| `top` | Correct shape, but every per-namespace `{time, count}` counter is 0 (no per-ns instrumentation) |
| `buildInfo` | `version` deliberately reports `"7.0.0"` (the MongoDB compatibility level drivers key feature gates on); the real package version is in `secantusVersion` |
| `serverStatus` | Real uptime / connections / opcounters / network metrics on the production server path; zeroed only for embedded callers that drive `CommandContext` directly without a metrics instance |
| `connectionStatus` | Real `authenticatedUsers` and `authenticatedUserRoles`; `authenticatedUserPrivileges` is left empty (mongod gates the per-action expansion behind a `showPrivileges` toggle we don't honour) |

`getLog`, `hostInfo`, and `whatsmyuri` return real data (in-memory log ring,
actual host OS/CPU/RAM, the requesting client's actual peer address). Logical
sessions are tracked in a real registry (`lsid` registration on every
command, 30-minute idle TTL, `endSessions` / `killSessions` honoured);
cursor → session affinity isn't tracked, so killing a session doesn't kill
its cursors — they age out under their own idle TTL.

`dbStats` and `collStats` return real `count`, `size`, `storageSize`,
`avgObjSize`, `indexSize`, `indexSizes`, `totalIndexSize`, and `totalSize`
computed from the WT tables.

`explain` reports `IXSCAN` when an index would be used and `COLLSCAN`
otherwise, with `indexName`, `keyPattern`, and `direction` populated.

## Stopgaps (functional but with limitations)

### `$lookup` join strategy

When the foreign collection has an index whose leading field is
`foreignField` (single-field, compound leading-prefix, or multikey), each
outer document's lookup rides that index (IXSCAN). Without a matching index
the join falls back to an O(N+M) in-memory hash table built once over the
foreign collection (array-valued local/foreign fields are covered via
element expansion). Both simple (`localField`/`foreignField`) and
`let`/`pipeline` forms work.

### `_id` numeric type bridge

Works for finite int / float / Decimal128 (they collide on equal value).
`bool` is deliberately not numeric. NaN and infinity `_id` values fall
through to the BSON-blob path; behaviour is unspecified.

### Date format strings

`$dateFromString` and `$dateToString` support mongod's format-token set —
`%Y %m %d %H %M %S %L %j %w %u %U %V %G %z %Z %%` — including the ISO-week
family and mongod's Sunday-first `%w` numbering. The `timezone` argument is
supported (IANA, UTC offsets, `GMT`/`UTC`).

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
| `expireAfterSeconds` | Honoured — a background sweeper prunes on a 60-second cadence (mongod's default; `ttl_sweep_seconds` configures it), and `Storage.prune_ttl` is callable directly with an injectable clock for deterministic tests |
| `partialFilterExpression` | Honoured at write time and at picker time |
| `collation` | Honoured at index-write and at picker time — strings are stored under collation-normalised bytes so a query carrying a matching `collation` lights up the index at IXSCAN. Strength 1/2/3 + `caseLevel` supported; `numericOrdering` not (needs a length-prefixed digit-run encoding — query falls back to COLLSCAN, results stay correct via `matches()`). See [Indexes](indexes.md) |

### Cursor TTL

Cursors idle longer than 600s are pruned opportunistically (matches
MongoDB's 10-minute cursor TTL). The clock is injectable via
`time_func` for deterministic tests.

## Deferred

(No entries — geo, native TLS / mTLS, MONGODB-X509, native checkpoint
backups, configuration file, per-write `j:true` routing, and
per-index collation all shipped in earlier slices. See the
sub-pages for the detail.)

## Out of scope

These are explicit non-goals:

- **Replica sets / sharding** — depend on multi-node cluster topology.
  SecantusDB is single-process. (Change streams *are* supported —
  oplog-backed and single-node — see [Change streams](change-streams.md).
  The oplog is queryable at `local.oplog.rs` like real mongod.)
- **Authentication mechanisms beyond SCRAM and MONGODB-X509** —
  LDAP, Kerberos, GSSAPI, MONGODB-AWS, MONGODB-OIDC. `SCRAM-SHA-256`,
  `SCRAM-SHA-1` (per-user opt-in), and `MONGODB-X509` (cert-as-username,
  over mTLS) *are* implemented, and authorization (RBAC — built-in and
  custom roles) is enforced when `--auth` is on. See
  [Authentication](authentication.md).
- **Native TLS + mTLS** are supported as of v0.5.1b21/b22.
  Configure via `[tls] cert_file` / `key_file` (server-side TLS)
  and optionally `[tls] ca_file` / `require_client_cert` (mTLS);
  see [Configuration](configuration.md). The `MONGODB-X509`
  cert-as-username auth mechanism shipped on top of the mTLS
  slice — see [Authentication](authentication.md).
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told
  the server doesn't support compression.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) —
  would need a full-text index implementation.
- **`$where` / `$function` / `$accumulator` / `mapReduce`** — all
  four evaluate user-supplied JavaScript and would need an embedded
  JS engine + sandbox + BSON↔JS shim layer. `mapReduce` is also
  explicitly deprecated by MongoDB; the canonical
  `emit(this.<field>, 1)` + `values.length` count pattern is
  recognised and translated to `$group`, but anything else needs
  real `mongod`.
What HAS shipped that's worth calling out (was previously listed as
"deferred" or "out of scope"): multi-document transactions (real
`commitTransaction` / `abortTransaction` with WT-native rollback,
snapshot isolation, write-conflict detection, and
`TransientTransactionError` labels — divergences in
`tasks/backlog.md` §3.4); geo operators + `2d` / `2dsphere`
indexes; capped collections (`create capped: true`) with FIFO
eviction; profiling (`profile` command + `<db>.system.profile`);
SCRAM-SHA-256 authentication; the oplog as a queryable
`local.oplog.rs` collection. See the [changelog](changelog.md) for
the full inventory and [aggregation](aggregation.md) /
[indexes](indexes.md) / [change streams](change-streams.md) for the
detail.

## Known edge cases

- **`$sample`** uses `random.sample` against fresh entropy per call
  by default. For deterministic test results set
  `SECANTUS_SAMPLE_SEED=<int_or_string>` in the environment — at
  module-import time `$sample` then uses a dedicated
  `random.Random(seed)` so the seed doesn't leak into the
  process-shared `random` state.
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
