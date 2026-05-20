# Aggregation

`aggregate` runs MongoDB pipeline stages against the collection. Stages are
applied in order; each stage gets the documents emitted by the previous one.

## Pipeline stages

| Stage | Notes |
| --- | --- |
| `$match` | Same matcher as `find()`; can be lifted into the initial fetch's filter for index acceleration when it's the first stage |
| `$count` | Single-doc result with the named field |
| `$limit` / `$skip` | Standard semantics |
| `$sort` | Multi-field BSON cross-type sort; uses index acceleration for single-field sorts where possible |
| `$project` | Inclusion / exclusion / computed fields. `$elemMatch` projection supported |
| `$addFields` / `$set` | Add / overwrite fields (computed via expressions) |
| `$unset` | Remove paths (string or array) |
| `$unwind` | Path string or doc form (`preserveNullAndEmptyArrays`, `includeArrayIndex`) |
| `$densify` | Numeric ranges and all date units (`week` / `day` / `hour` / `minute` / `second` / `millisecond` via timedelta; `month` / `quarter` / `year` via `dateutil.relativedelta`, so day-snap on month/year roll-overs matches mongod) |
| `$fill` | `{value: <expr>}` evaluates per-doc; `{method: "locf"}` carries last observation forward; `{method: "linear"}` interpolates between bracketing non-null anchors along `sortBy` (numbers + datetimes). Partition via `partitionBy` / `partitionByFields`; `sortBy` required when any output uses `method` |
| `$replaceRoot` / `$replaceWith` | Replace the root with a sub-document |
| `$group` | See accumulators below |
| `$lookup` | Both simple (`localField`/`foreignField`) and `let`/`pipeline` forms. Uses an index-driven path when the foreign collection has an index whose leading field is `foreignField`; otherwise an O(N+M) hash-join (foreign array values expanded element-wise) |
| `$sample` | `random.sample` — deterministic only if the test calls `random.seed(...)` first |
| `$sortByCount` | Equivalent to `$group` + `$sort` |
| `$facet` | Run multiple sub-pipelines in parallel |
| `$bucket` / `$bucketAuto` | `groupBy`, `boundaries`, `default`, `output` (bucket); `groupBy`, `buckets`, `output` (bucketAuto) |
| `$merge` | `into` is a string or `{db, coll}`. `whenMatched`: `"merge"` (deep recursive merge, default), `"replace"`, `"keepExisting"`, `"fail"`, `"delete"` (5.0+), or `[<sub-pipeline>]` with `$$new` binding + user `let` vars. `whenNotMatched`: `"insert"` (default), `"discard"`, `"fail"`. `on` non-`_id` fields require a `unique: true` index covering them (mongod's rule) |
| `$out` | Replace target collection with pipeline output |
| `$collStats` / `$indexStats` | Count + size metrics; capped bounds (`storageStats.{capped, max, maxSize}`) surface for capped collections |
| `$currentOp` / `$listLocalSessions` / `$listSessions` | Session and operation enumeration |
| `$geoNear` | Spherical (`2dsphere`) or planar (`2d`); auto-picks the geo index when one exists, falls back to a full-scan distance computation otherwise. See [Indexes](indexes.md) for the geo-index path |
| `$graphLookup` | Recursive lookup with `maxDepth` |
| `$documents` | Inline document source (5.1+) |
| `$changeStream` | Pipeline-form change-stream entry point |
| `$unionWith` | Concatenate docs from another collection. Shorthand `{$unionWith: "coll"}` or full form `{$unionWith: {coll, pipeline}}` with an optional sub-pipeline that runs in a fresh context (outer `let`/vars are not visible). Outer docs first, then union docs; no deduplication |
| `$redact` | Content-based document / sub-document pruning. Expression must return `"$$KEEP"`, `"$$PRUNE"`, or `"$$DESCEND"`. Top-level `$$PRUNE` drops the doc; `$$DESCEND` recurses into every dict-valued field and every dict-valued list element; `$$KEEP` short-circuits descent. Non-sentinel return raises `AggregateError` |

### `$group` accumulators

`$sum`, `$count`, `$avg`, `$min`, `$max`, `$first`, `$last`, `$push`,
`$addToSet`. Group buckets are emitted in first-seen order (matches
unsharded MongoDB; sharded behaviour isn't modelled).

## Expression operators

The `$expr` operator inside `$match`, plus computed fields in `$project` /
`$addFields`, runs through a single expression evaluator
(`secantus.expressions.evaluate`).

### Field paths and variables

- `"$x.y"` — path into the current doc.
- `"$$ROOT"` / `"$$CURRENT"` — current doc.
- `"$$varname"` — user variable (set via `$let`, `$lookup`'s `let`, or
  `$merge`'s `let`).
- `"$$ROOT.field.path"` / `"$$varname.field.path"` — walk a dotted path
  into a resolved variable (useful inside `$merge`'s `whenMatched`
  sub-pipeline for `$$new.field`).
- `{$literal: ...}` — bypass field-path / operator interpretation.
- `"$$REMOVE"` — sentinel that removes the field when used as a
  computed-field value in `$project` / `$addFields` / `$setField`.

### Arithmetic and comparison

`$add`, `$subtract`, `$multiply`, `$divide`, `$mod`, `$abs`, `$ceil`,
`$floor`, `$exp`, `$ln`, `$log`, `$log10`, `$pow`, `$sqrt`, `$round`,
`$trunc`. Comparison: `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$cmp`.

### Logical and conditional

`$and`, `$or`, `$not`, `$cond` (dict or array form), `$ifNull`, `$switch`,
`$let`.

### Strings

`$concat`, `$split`, `$trim`, `$ltrim`, `$rtrim`, `$substrCP`, `$strLenCP`,
`$indexOfCP`, `$toLower`, `$toUpper`, `$toString`, `$regexMatch`,
`$regexFind`, `$regexFindAll`.

### Arrays

`$size`, `$arrayElemAt`, `$first`, `$last`, `$slice`, `$concatArrays`,
`$reverseArray`, `$in`, `$filter`, `$map`, `$reduce`, `$range`, `$zip`,
`$arrayToObject`, `$objectToArray`.

### Documents

`$mergeObjects`, `$objectToArray`, `$arrayToObject`, `$setField`,
`$getField`, `$unsetField`.

### Dates

`$year`, `$month`, `$dayOfMonth`, `$dayOfWeek`, `$hour`, `$minute`,
`$second`, `$millisecond`, `$dateToString`, `$dateFromString`.

`$dateToString` and `$dateFromString` accept a **timezone** argument:

- IANA names: `"Europe/Dublin"`, `"America/New_York"` (via `zoneinfo`).
- UTC offsets: `"+05:30"`, `"-04:00"`, `"+0530"` (fixed-offset tzinfo).
- Aliases: `"UTC"`, `"GMT"`, `"Etc/UTC"`, `"Etc/GMT"`.

`$dateToString`: input datetime is treated as UTC if naive (matching BSON
Date semantics) and converted to the requested zone before formatting.

`$dateFromString`: when the parsed string has no zone info, the requested
timezone becomes the input's tzinfo, so the returned datetime represents
the correct UTC instant.

Unknown timezone names raise an error (no silent misinterpretation).

### Type checks and conversions

`$type`, `$toInt`, `$toLong`, `$toDouble`, `$toBool`, `$toDecimal`,
`$toString`, `$toObjectId`, `$toDate`. The `$type` operator returns the
BSON type alias for a value; the int32-vs-int64 distinction depends on
Python value range rather than the original BSON tag (which we throw away
on decode).

## What's not supported

- `$where` / `$function` / `$accumulator` — all three evaluate
  user-supplied JavaScript and would need an embedded JS engine,
  sandbox, and BSON↔JS shim layer. SecantusDB doesn't ship a JS
  runtime; not on the roadmap (see `tasks/backlog.md` §4 for the
  rejected `lang: "python"` alternative and the trusted-plugin
  escape hatch).
- `mapReduce` — same JS-runtime dependency; also explicitly
  deprecated by MongoDB (removed from the Stable API in 5.0). The
  canonical `emit(this.<field>, 1)` + `values.length` "count by
  field" pattern is recognised and translated to an equivalent
  `$group` aggregation; non-canonical bodies return
  `{results: [], ok: 1}` so wire-shape probes pass.
- Text search (`$text`, `$meta: "textScore"`) — would need a full-text
  index implementation.

## Pipeline tips

- Put `$match` first so it can be lifted into the initial fetch's filter
  and benefit from index acceleration.
- `$lookup` joins are O(N+M) in memory; use `$match` before `$lookup` to
  shrink the outer side.
- `$sort` followed by `$limit` is NOT yet a single optimised stage — sort
  runs the full collection then limit truncates. For test workloads this
  is fine; for large simulated datasets, prefer to sort by an indexed
  field so the sort itself is index-walked.
- For deterministic `$sample` results in tests, set
  `SECANTUS_SAMPLE_SEED=<int_or_string>` in the environment. The seed
  is captured at module import; `$sample` then uses a dedicated
  `random.Random(seed)` instead of the process-shared `random`
  module, so seeding here doesn't leak into other code in the same
  process.
