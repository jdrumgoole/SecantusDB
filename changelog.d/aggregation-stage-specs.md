### Aggregation stages validate their specs, and four BSON types became queryable again

A sweep of every aggregation **stage** crossed with every pathological argument —
725 shapes, a surface no earlier sweep covered — found 167 divergences against
mongod 8.2.11. It is now 22, and everything left is a message or
validation-order difference rather than a wrong answer.

#### Fixed

- **`$unset` validated nothing.** A non-string, non-array spec raised a bare
  `TypeError` and reached the client as `internal server error`; an empty string
  and an empty array were accepted and did nothing; a document spec silently
  iterated its *keys*. mongod has four distinct codes for those.
- **`$out: ""` and `$merge: ""` were accepted**, writing to a nameless
  collection. Both are `InvalidNamespace` on mongod. Neither stage validated its
  field names either, so a typo was silently ignored — and `$out` takes
  `{db, coll}` where `$merge` takes `{into: ...}`.
- **`$documents` is a collection-less stage.** Run against a collection, mongod
  refuses it before looking at the argument at all; we ran it happily.
- **Four BSON types were unreachable by `$type`.** `javascript`,
  `javascriptWithScope`, `timestamp`, `minKey` and `maxKey` all validated as
  aliases but had no predicate, so the query silently matched nothing — and
  `$type: "string"` matched JavaScript values.
- **`$group` on a field holding a `bson.Code` crashed**, as did `$count`, `$out`
  and `$merge` given one as their spec, and `$unset` silently accepted one.
  `Code` subclasses `str` and defines `__eq__` without `__hash__`;
  `bsontypes.is_bson_string` is now the check to use where the distinction is a
  BSON one.
- `$set` reported itself as `$addFields`, whose handler it shares.

#### Changed

- mongod has **two** value renderings and they are not interchangeable: they
  differ in six places (`[ 1 ]` vs `[1]`, `BinData(0, 7A)` vs
  `BinData(0, "7A")`, `ObjectId('…')` vs bare hex, `new Date(ms)` vs ISO-8601,
  and two more). Everything else is identical, which is what made the difference
  easy to miss. `bsontypes` now holds one function per family, retiring four
  more partial copies.
