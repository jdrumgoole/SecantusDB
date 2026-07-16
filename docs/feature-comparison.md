# Feature comparison

A three-way feature matrix: **MongoDB** (real `mongod`, the conformance
target — SecantusDB vendors the same WiredTiger storage engine and
advertises wire version 7.0) against SecantusDB's **Python server** and
**Rust server** (see [The two servers](servers.md)).

The honest, machine-checked statement of conformance is the
[validation reports](validation-summary.md) — the official driver test
suites (pymongo, Go, Node, Java, Kotlin, Ruby, Rust, PHP library +
extension, C, C++, .NET) run unmodified against SecantusDB. This page is
the human-readable decomposition: which features each server implements,
where they diverge from `mongod`, and where they diverge from each other.

Legend: ✅ supported · ⚠️ partial (see note) · ❌ not supported.
The MongoDB column describes `mongod` 7.0 itself and is the reference.

## At a glance

| | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| What it is | the real thing | pure-Python in-process server | self-contained Rust server, GIL-free request path |
| Install | external `mongod` process | `pip install secantus` | storage-engine-flag build / `pip install "secantus[rust]"` |
| Run | daemon | 1-line embedded (`SecantusDBServer`) or `secantusd-py` daemon | embedded handle (`RustServer`) or `secantusd-rs` daemon |
| pymongo's own suite | 100% by definition | **99.5%** (1020 pass / 5 fail) | **99.5%** (1020 pass / 5 fail) |
| Version line | 7.0.x | `0.5.4bN` (the PyPI package) | `0.5.3-beta.N` (crates, `buildInfo.secantusVersion`) |

## Deployment and topology

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| Single-node standalone | ✅ | ✅ | ✅ |
| Embedded in-process (no external process) | ❌ | ✅ | ✅ (Rust accept loop in-process, Python is only the launcher) |
| Storage engine | WiredTiger | WiredTiger (vendored, same version line) | WiredTiger (vendored, same version line) |
| In-memory mode | enterprise only | ✅ (`:memory:`) | ✅ |
| Replica sets (multi-node, elections) | ✅ | ❌ — single-node primary *persona* only, so drivers accept change streams | ❌ — same persona |
| Sharding / `mongos` | ✅ | ❌ | ❌ |
| Windows / macOS / Linux | ✅ | ✅ (prebuilt wheels, no toolchain needed) | ⚠️ macOS / Linux; Windows binary deferred |

## Wire protocol

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| `OP_MSG` (incl. kind-1 document sequences) | ✅ | ✅ | ✅ |
| Legacy `OP_QUERY` handshake | ✅ | ✅ | ✅ |
| `OP_COMPRESSED` | ✅ | ❌ | ❌ |
| Wire-valid error replies (`ok: 0`, `code`, `codeName`) | ✅ | ✅ | ✅ |
| Unknown command → `CommandNotFound` (59), connection survives | ✅ | ✅ | ✅ |

## Commands

Both servers implement the same core dispatch surface: CRUD
(`insert` / `find` / `update` / `delete` / `count` / `distinct` /
`findAndModify` / `getMore` / `killCursors` / `aggregate` / `explain`), DDL
(`create` / `drop` / `collMod` / `dropDatabase` / `renameCollection` /
`listCollections` / `listDatabases` / index commands), diagnostics
(`hello` / `ping` / `buildInfo` / `serverStatus` / `dbStats` / `collStats` /
`validate` / `getLog` / `currentOp` / `killOp` / `fsync` / `profile` /
`configureFailPoint`), sessions / transactions, and the full auth / RBAC
command set. The divergences:

| Command | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| `mapReduce` | ⚠️ deprecated | ⚠️ `{out: {inline: 1}}` only; canonical emit/count patterns translated (no JS runtime) | ❌ |
| `top` | ✅ | ⚠️ correct shape, counters always 0 | ❌ |
| `serverStatus` | ✅ | ⚠️ version + mostly-zeroed metrics | ⚠️ smaller subset still |
| `dbStats` / `collStats` | ✅ | ✅ real counts/sizes from the WT tables | ⚠️ `dataSize` used for size fields |
| `replSetGetStatus` | ✅ | ⚠️ single-node persona reply | ⚠️ single-node persona reply |
| Atlas Search index commands (`createSearchIndexes`, …) | Atlas only | ❌ rejected (`CommandNotSupported`) | ❌ rejected |
| `secantusAdmin.*` (backup / PITR / prune — SecantusDB-proprietary) | — | ✅ | ✅ (`restoreToTimestamp` via the `secantusd-rs restore` CLI rather than a wire command) |

## Query operators

Identical surface on both servers: all comparison (`$eq` `$ne` `$gt` `$gte`
`$lt` `$lte` `$in` `$nin`), logical (`$and` `$or` `$nor` `$not`), element
(`$exists` `$type`), evaluation (`$mod` `$regex` `$expr` `$jsonSchema`
`$comment`), array (`$all` `$elemMatch` `$size`), bitwise (`$bitsAllSet`
`$bitsAnySet` `$bitsAllClear` `$bitsAnyClear`), and geo (`$geoWithin`
`$geoIntersects` `$near` `$nearSphere` with `$geometry` / `$box` / `$center` /
`$centerSphere` / `$polygon` / `$maxDistance` / `$minDistance`) operators.

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| `$where` (JavaScript) | ✅ | ❌ (no JS runtime — out of scope) | ❌ |
| `$text` | ✅ | ❌ (no text indexes) | ❌ |
| `$jsonSchema` | ✅ | ⚠️ full keyword set except `$ref` and metadata keywords | ⚠️ same |

## Update operators

No known divergences from `mongod`'s operator list, in either server: `$set`
`$setOnInsert` `$unset` `$inc` `$mul` `$min` `$max` `$rename` `$bit`
`$currentDate` `$push` (with `$each` / `$position` / `$slice` / `$sort`)
`$addToSet` (with `$each`) `$pull` `$pullAll` `$pop`; positional `$` / `$[]` /
`$[<identifier>]` with `arrayFilters`; and pipeline-form updates (`$set` /
`$addFields` / `$unset` / `$project` / `$replaceRoot` / `$replaceWith`).

## Aggregation stages

Both servers: `$match` `$project` `$addFields`/`$set` `$unset` `$replaceRoot`
`$replaceWith` `$redact` `$limit` `$skip` `$sort` `$count` `$unwind` `$group`
`$sortByCount` `$bucket` `$bucketAuto` `$densify` (incl. `month` / `quarter` /
`year` calendar units) `$fill` `$setWindowFields` `$lookup` `$graphLookup`
`$facet` `$sample` `$out` `$merge` `$unionWith` `$geoNear` `$documents`
`$collStats` `$indexStats` `$changeStream` `$changeStreamSplitLargeEvent`
`$currentOp` `$listLocalSessions` `$listSessions` (the session/op stages emit
synthesized rows on both).

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| `$search` / `$searchMeta` / `$vectorSearch` / `$listSearchIndexes` | Atlas only | ❌ rejected cleanly | ❌ rejected cleanly |
| Leading-`$match` index acceleration | ✅ | ✅ | ✅ |
| `$lookup` index-driven join | ✅ | ✅ (foreign-field index → IXSCAN, else hash join) | ✅ |

## `$group` accumulators and window functions

Both servers: `$sum` `$avg` `$min` `$max` `$first` `$last` `$push` `$addToSet`
`$count` `$stdDevPop` `$stdDevSamp` `$mergeObjects` `$top` `$bottom` `$topN`
`$bottomN` `$firstN` `$lastN` `$maxN` `$minN`; window functions `$rank`
`$denseRank` `$documentNumber` `$shift` `$expMovingAvg` `$locf` `$linearFill`
`$derivative` `$integral` plus the accumulators over document- and range-based
windows (fixed-duration time units; calendar units `month` / `quarter` /
`year` are not accepted in window bounds on either server).

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| `$median` / `$percentile` | ✅ | ❌ | ❌ |
| `$accumulator` (JS) | ✅ | ❌ (no JS runtime) | ❌ |

## Expression operators

Both servers implement the expression language near-completely: arithmetic
(incl. `$rand`, `$log10`, all trigonometry `$sin` … `$atan2` / `$sinh` … and
`$degreesToRadians` / `$radiansToDegrees`), comparison (incl. `$cmp`),
logical, conditional (`$cond` `$ifNull` `$switch`), type introspection
(`$type` `$isNumber` `$isArray`), string (incl. `$replaceOne` /
`$replaceAll`, `$regexMatch` / `$regexFind` / `$regexFindAll`, byte and
code-point variants), array (incl. `$sortArray` `$zip` `$reduce`
`$firstN` / `$lastN` / `$maxN` / `$minN`), set (`$setUnion`
`$setIntersection` `$setDifference` `$setEquals` `$setIsSubset`
`$anyElementTrue` `$allElementsTrue`), object (`$mergeObjects`
`$objectToArray` `$arrayToObject` `$getField` `$setField`), date
(`$dateAdd` `$dateSubtract` `$dateDiff` `$dateTrunc` `$dateFromParts`
`$dateToParts` `$dateToString` `$dateFromString` and every getter incl.
`$millisecond` / `$week` / `$dayOfYear` / the ISO family), conversion
(`$convert`, `$toInt` / `$toDouble` / `$toBool` / `$toDecimal` /
`$toString` / `$toDate`), timestamps (`$tsSecond` `$tsIncrement`), BSON
introspection (`$binarySize` `$bsonSize`), bitwise (`$bitAnd` `$bitOr`
`$bitXor` `$bitNot`), and `$let` / `$literal` with the system variables
(`$$ROOT` `$$CURRENT` `$$NOW` `$$REMOVE`, `$redact`'s `$$KEEP` / `$$PRUNE` /
`$$DESCEND`).

Missing from **both** servers (error cleanly):

- the accumulator family in expression position — `$sum` / `$avg` / `$min` /
  `$max` / `$stdDevPop` / `$stdDevSamp` over an array argument in `$project` /
  `$addFields` (they work as `$group` / window accumulators)
- `$median` / `$percentile` (expression form)
- `$meta`, `$toLong`, `$toObjectId`
- `$function` (no JS runtime)

Rust-server-only limitations: a handful of edge cases the Rust engine
rejects rather than risk diverging from the Python oracle — some
`$dateFromString` / `$dateToString` format directives, Decimal128 arithmetic
edges, and sorts over non-totally-orderable value mixes (NaN / bool /
Decimal128).

Unknown expression operators report mongod's error codes: a query `$expr`
returns `InvalidPipelineOperator` (168) on both servers, and a `$project`
returns mongod's `Location31325` shape on the Python server (the Rust server
still returns a generic `BadValue` for the `$project` case).

## Indexes

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| Single-field + compound, mixed ASC/DESC | ✅ | ✅ | ✅ |
| Unique (E11000 dup-key message shape) | ✅ | ✅ | ✅ |
| Sparse | ✅ | ✅ | ✅ |
| Partial (`partialFilterExpression` + planner implication) | ✅ | ✅ | ✅ |
| TTL (`expireAfterSeconds`, background sweeper) | ✅ | ✅ | ✅ |
| Multikey (array) | ✅ | ✅ | ✅ |
| Per-index collation | ✅ | ⚠️ strength 1–3 + `caseLevel`; `numericOrdering` → COLLSCAN | ⚠️ same |
| Hidden indexes | ✅ | ✅ | ✅ |
| Geo `2dsphere` (S2 coverings) / `2d` (geohash) | ✅ | ✅ | ✅ |
| Text | ✅ | ❌ rejected | ❌ rejected |
| Hashed | ✅ | ❌ rejected | ❌ rejected |
| Wildcard (`$**`) | ✅ | ❌ | ❌ |
| `hint` (name, key spec, `$natural`) | ✅ | ✅ | ✅ |
| `explain` (IXSCAN/COLLSCAN + `winningPlan` shape) | ✅ | ✅ | ✅ |

## Collections

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| Capped collections (FIFO eviction, tailable cursors) | ✅ | ✅ | ✅ |
| Timeseries collections | ✅ | ✅ (duplicate `_id`s handled) | ✅ |
| Clustered collections (`{_id: 1}`) | ✅ | ✅ | ✅ |
| Schema validation (`validator`, `validationLevel` / `validationAction`) | ✅ | ✅ | ✅ |
| Views (`viewOn` + pipeline, incl. views-on-views) | ✅ | ✅ | ✅ |

## Change streams

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| Collection / database / cluster scope | ✅ | ✅ | ✅ |
| `fullDocument: updateLookup` / `whenAvailable` / `required` | ✅ | ✅ | ✅ |
| Pre-images (`fullDocumentBeforeChange`) | ✅ | ✅ | ✅ |
| `resumeAfter` / `startAfter` / `startAtOperationTime` | ✅ | ✅ | ✅ |
| `showExpandedEvents` (DDL events) | ✅ | ✅ | ✅ |
| `splitLargeChangeStreamEvents` / `$changeStreamSplitLargeEvent` | ✅ | ✅ | ✅ |
| Invalidate on drop / dropDatabase / rename | ✅ | ✅ | ✅ |
| Resume tokens interchangeable with real mongod | ✅ | ❌ SecantusDB's own `{s,t,n,k}` layout — round-trips within SecantusDB only | ❌ same layout (interchangeable between the two servers) |

## Transactions and sessions

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| Multi-document transactions (WT-backed, single-node) | ✅ | ✅ | ✅ |
| `commitTransaction` / `abortTransaction` (idempotency, error labels, `NoSuchTransaction`, write conflicts) | ✅ | ✅ | ✅ |
| Read/write concern | ✅ | ⚠️ accepted and ignored (single node — no majority/snapshot semantics) | ⚠️ same |
| `readConcern: snapshot` timestamp pinning | ✅ | ⚠️ accepted, reads not actually pinned | ⚠️ same |
| Retryable writes | ✅ | ⚠️ envelope tolerated, no dedup by `txnNumber` | ⚠️ same |
| Logical sessions (`lsid` tracking, 30-min TTL, kill/refresh) | ✅ | ✅ | ⚠️ `startSession` real; end/refresh/kill acknowledged as no-ops |

## Authentication and transport security

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| SCRAM-SHA-256 / SCRAM-SHA-1 | ✅ | ✅ | ✅ |
| MONGODB-X509 | ✅ | ✅ | ✅ |
| TLS / mTLS (client-cert verification) | ✅ | ✅ | ✅ (rustls) |
| RBAC: built-in roles, custom roles, grant/revoke, privileges | ✅ | ✅ | ✅ |
| LDAP / Kerberos / PLAIN / MONGODB-AWS / MONGODB-OIDC | ✅ (some enterprise) | ❌ | ❌ |
| Internal cluster auth (keyfile) | ✅ | ❌ (no cluster) | ❌ |

## Backup and point-in-time recovery

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| Oplog (single-node, retention + pruning) | ✅ | ✅ | ✅ |
| Hot backup archive | ✅ (backup cursor / ops tooling) | ✅ `secantusAdmin.backupArchive` / `archiveBaseSnapshot` | ✅ same commands |
| Restore an archive | ✅ | ✅ `secantusAdmin.restoreArchive` / `secantus-restore-archive` | ✅ |
| Point-in-time restore (oplog replay) | ✅ (ops tooling) | ✅ `secantusAdmin.restoreToTimestamp` | ⚠️ `secantusd-rs restore` CLI; no wire command yet |
| Archives portable between the two servers | — | ✅ | ✅ |

## Beyond MongoDB: the SQL/PostgreSQL frontend

The Python server also speaks the **PostgreSQL wire protocol**
(`secantusd-py-pg`) against the same WiredTiger data — `psql`, psycopg, or
SQLAlchemy connect to the same store MongoDB clients use. See
[SQL / PostgreSQL interface](sql.md).

| Feature | MongoDB | Python server | Rust server |
| --- | --- | --- | --- |
| PostgreSQL v3 wire protocol (simple + extended query) | ❌ | ✅ | ❌ |
| SQL engine (planner / executor / catalog / window functions / `EXPLAIN`) | ❌ | ✅ | ❌ |
| PG types & features (bytea, ranges, intervals, hstore, jsonpath, tsvector/tsquery FTS, LISTEN/NOTIFY, RLS, GRANT/REVOKE, plpgsql functions) | ❌ | ✅ | ❌ |
| PG SCRAM-SHA-256 auth + TLS | ❌ | ✅ (no channel binding) | ❌ |

## Out of scope for all SecantusDB servers

Anything that depends on real cluster topology or an embedded JavaScript
runtime is out of scope by design, in both servers: multi-node replica sets,
elections, sharding, cross-node oplog; `$where`, `$function`, `$accumulator`,
JS `mapReduce`; text / hashed / wildcard indexes; `OP_COMPRESSED`; Atlas
Search / Vector Search; LDAP / Kerberos / AWS / OIDC auth. See
[Compatibility](compatibility.md) for the per-feature detail and
[the validation reports](validation-summary.md) for the machine-checked
conformance numbers this page summarizes.
