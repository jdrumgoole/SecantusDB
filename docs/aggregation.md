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
| `$densify` | Numeric ranges only (`bounds: "full"` / `[min, max]`, `partitionByFields`, positive `step`); date `unit` deferred |
| `$replaceRoot` / `$replaceWith` | Replace the root with a sub-document |
| `$group` | See accumulators below |
| `$lookup` | Both simple (`localField`/`foreignField`) and `let`/`pipeline` forms; uses an O(N+M) hash-join (foreign array values expanded element-wise) |
| `$sample` | `random.sample` — deterministic only if the test calls `random.seed(...)` first |
| `$sortByCount` | Equivalent to `$group` + `$sort` |
| `$facet` | Run multiple sub-pipelines in parallel |
| `$bucket` | `groupBy`, `boundaries`, `default`, `output` |
| `$merge` | `whenMatched`: `merge` (deep recursive merge), `replace`, `keepExisting`, `fail`. `whenNotMatched`: `insert`, `discard`, `fail`. `into` may be a string or `{db, coll}` |
| `$out` | Replace target collection with pipeline output |
| `$collStats` | Returns count + size metrics from the WT tables |
| `$graphLookup` | Recursive lookup with `maxDepth` |

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
- `"$$varname"` — user variable (set via `$let` or `$lookup`'s `let`).
- `{$literal: ...}` — bypass field-path / operator interpretation.

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

- `$where` — needs a JavaScript runtime; out of scope.
- `$function` — same reason.
- `$densify` with `unit` (date ranges) — deferred.
- `$fill` — deferred.
- `mapReduce` — deprecated by MongoDB; not implemented.
- Text search (`$text`, `$meta: "textScore"`) — would need a full-text
  index implementation.
- Geo (`$near`, `$geoWithin`, ...) — would need geometric primitives.

## Pipeline tips

- Put `$match` first so it can be lifted into the initial fetch's filter
  and benefit from index acceleration.
- `$lookup` joins are O(N+M) in memory; use `$match` before `$lookup` to
  shrink the outer side.
- `$sort` followed by `$limit` is NOT yet a single optimised stage — sort
  runs the full collection then limit truncates. For test workloads this
  is fine; for large simulated datasets, prefer to sort by an indexed
  field so the sort itself is index-walked.
