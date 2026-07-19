# mongod-fidelity divergence catalog (2026-07-18)

Found by a parallel read-only probe sweep (3 discovery agents, mongod 7.0.12 on
distinct ports vs SecantusDB's pure Python engines). Each row is a **confirmed**
behavioural divergence. This is the working queue for the type-coercion / argument-
validation fidelity effort. Tiers are by severity; burn down Tier 1 first. When a
row is fixed, delete it (three-way mongod-verify + both engines + parity as usual).

The already-shipped sweep (bool-reject, whole-double accept, substr numeric args,
`$limit`/`$skip`/`$sample`, `$bits*`) is NOT listed here — those agree with mongod.

## Tier 1 — crash / data-corruption / silent-wrong-results (highest value)

- ~~**`$pow` returns a Python complex**~~ **FIXED (#483):** returns NaN + operand
  validation (28762/28763/28764).
- ~~**`$gte`/`$lte: null`** doesn't match null+missing~~ **FIXED (#484):** routes to
  `$eq: null` semantics, both engines.
- ~~**`$exists` Python-truthiness**~~ **FIXED (#484):** uses mongod truthiness
  (empty string/array/doc truthy), both engines.
- ~~**`$rename` data corruption**~~ **FIXED (#485):** rejects array-element source/
  dest, same-field, same-path, empty target (56), and non-string target — no more
  silent corruption / AttributeError leak. Both engines.
- ~~**`$toInt`/`$convert` int32/int64 overflow**~~ **FIXED (#489):** `$toInt` /
  `$convert` (int/long) error on out-of-range / non-finite (241, `onError`-caught)
  instead of returning an unbounded / silently-widened int; `$toInt` narrows int64
  input to int32 like mongod. Both engines. (`$toLong` shipped in #529 — a
  separate missing-operator item, see Tier 2.)
- ~~**`$bucket` silent data loss**~~ **FIXED (#487):** out-of-range value with no
  default now errors (7158303) instead of dropping the doc; full spec validation
  added (40192-40200). Both engines. (The `$bucketAuto` / unsorted-etc. Tier-2
  `$bucket` rows below are also covered.)
- **`$gte`/`$lte: null`** doesn't match null+missing (mongod matches, like `$eq:null`)
  → wrong query results. **Verified.** (query.py / .rs)
- **`$exists` Python-truthiness** (`""`/`[]`/`{}` treated falsy; mongod: only
  `false`/`0`/`null` are falsy) → wrong results. **Verified.** (query.py / .rs)
- ~~**`$group` accumulator coerces string→number**~~ **FIXED (#491):** `$sum`/`$avg`
  ignore non-numeric operands (string/bool/null/missing) — all-non-numeric group →
  `0` / `null`; `$min`/`$max` order mixed types by BSON cross-type order and skip
  null/missing instead of raising. Both engines. (Rust min/max over mixed types now
  computes via `order::bson_lt` rather than deferring.)
- ~~**Uncaught Python exceptions leak (`code=None`)**~~ **ALL FIXED** — every listed
  operator now raises mongod's code on both engines instead of leaking a Python
  exception. `$pow` (#483); unary math `$abs`/`$ceil`/`$floor`/`$sqrt`/`$exp`/`$ln`/
  `$log10`/`$round`/`$trunc` (#492, → 28765/51081); `$log` (#493, → 28756/28757);
  `$in`/`$nin` (#494); `$regex`/`$options` (#496, → 51108/BadValue); `$not`/
  `$elemMatch` (#497); `$split` (#500, → 40085/40086/40087/16020); `$sort` stage
  (#502, → 15974/15975/15976); `$densify` (#504, → 6053600/14/5733401/5946802/
  5733403/5733402); `$facet` (#505, → 40169/40170/40171/40600).

## Tier 2 — silent-accept of invalid input / missing type guards

- ~~**Math-unary bool + non-numeric**~~ **FIXED** (`$abs`/`$ceil`/`$floor`/`$sqrt`/
  `$exp`/`$ln`/`$log10`/`$round`/`$trunc` → #492 [28765/51081], `$pow` → #483
  [28762/28763/28764], `$log` → #493 [28756/28757]). Whole math-operator family now
  type-guards both engines. (expressions)
- **Array/string type guards**: `$concat` non-string FIXED (#509, → 16702); non-array
  to `$reverseArray`/`$concatArrays`/`$map`/`$filter`/`$reduce`/`$first`/`$last`/`$slice`
  FIXED (#513, → 34435/28664/16883/28651/40080/28689/28724; null → null); `$trim`/
  `$ltrim`/`$rtrim` non-string input/chars FIXED (#515, → 50699/50700; chars:null →
  null); `$indexOfBytes`/`$indexOfCP` start/end FIXED (#518, → 40096/40097; whole
  double accepted); `$toDate` bool → 241 FIXED (#519; onError-catchable, both
  engines). **Array/string type-guard cluster complete.** (expressions)
- **Date-arg whole-double / bool**: `$dateAdd`/`$dateSubtract` amount + `$dateTrunc`
  binSize FIXED (#521, → whole-double accepted; fractional/bool → 5166405/5439017;
  binSize <1 → 5439018). `$toLong` FIXED (#529, → implemented as the int64
  counterpart of `$toInt`; truncates 2.7→2, parses strings, overflow → 241).
- **Query operator validation**: `$in`/`$nin` non-array + `$`-doc element FIXED (#494,
  → 2); `$regex` bad `$options` / `$options`-without-`$regex` / non-string `$regex`
  FIXED (#496, → 51108 / 2); `$elemMatch` non-doc (query) + `$not` invalid arg
  FIXED (#497, → 2); `$all` non-array + mixed/non-`$elemMatch` `$`-doc FIXED (#499,
  → 2). `$elemMatch` non-doc in *projection* FIXED (#526, → 31274). (query.py)
- ~~**Projection `$slice`**~~ **FIXED (#506):** non-number scalar / short / long
  array → 28667; 2/3-elem array not `[skip, positive-limit]` → 28724. Both engines
  (Rust core defers). (projection.py)
- ~~**`$type` validation**~~ **FIXED (#508):** unknown alias / out-of-range / fractional
  code → 2 (code-0 hint), bool → 14; the Rust engine now computes valid whole-double
  codes instead of deferring. Valid alias set = 22, code set = {-1,1..19,127}. Both
  engines. (query.py / .rs)
- **Update `$push $sort`** bad spec (scalar not ±1 / `{field: bad}` / non-numeric) FIXED
  (#527, → 2; whole-double ±1 now accepted). `$pull`/`$pullAll` on a present non-array
  field FIXED (#526, → 2; missing field stays a no-op). `$currentDate: {d:false}` FIXED
  (#527, → accepts bool false as set-Date; bad scalar / bad `$type` → 2). `arrayFilters`
  non-object (14) / empty (9) / bad identifier (2) / duplicate (9) / unused (9) FIXED
  (#528); nested-`$and`/`$or`/`$nor` identifier extraction + single-identifier rule
  (two idents → 9, `$expr` → 224) FIXED (#531, both engines). (update.py)
- **Stages**: `$bucketAuto` buckets bool/non-numeric/fractional/
  non-positive/missing FIXED (#526, → 40241/40242/40243/40246; whole-double accepted);
  `granularity` non-string/unknown FIXED (#530, → 40261/40257; a valid series is rejected
  as unsupported — byte-exact boundary rounding is fp-blocked, see backlog); `$densify`
  `{step:true}`/`bounds:'partial'`/`[0]`/`[5,0]`/unit-on-numeric FIXED (#504, →
  14/5946802/5733403/5733402/6053600; `{step:1.5}` is mongod-valid, now computes);
  `$unwind` path/includeArrayIndex/preserve FIXED (#523, → 28808/28818/28810/28822/
  28809); `$sortByCount`
  number/non-expr-doc/no-`$` FIXED (#525, → 40149/40147/40148); `$facet` empty/non-array/
  non-object-stage/nested FIXED (#505, → 40169/40170/40171/40600); `$count`
  ''/dotted/`$`-prefixed/`_id`/non-string FIXED (#525, → 40157/40160/40158/15948/40156);
  `$sort` bad-ordering/empty/`{v:true}` FIXED (#502, → 15975/15976/15974); `$project {}`
  empty FIXED (#525, → 51272). (aggregate.py)

## Tier 3 — wrong error code (both error; SecantusDB returns 14 or None vs a Location code)

Large, low-value count. Python raises but with code 14 (TypeMismatch) or None where
mongod uses a specific Location code — the Rust server already renders BadValue for
all of these (the standing error-code gap). Fixed on the Python server:
`$strLenBytes`/`$strLenCP` non-string (34473/34471), `$sortArray` non-array (2942504),
`$toInt`/`$toDouble`/`$toDecimal`/`$toLong` non-numeric string (241), `$convert to:bad` (2,
uncatchable by `onError`) — #532; `$zip` non-array inputs/element (34461/34468),
`$arrayToObject` non-array (40386), `$objectToArray` non-document (40390), `$replaceOne`/
`$replaceAll` per-argument (51746/51745/51744), `$dateDiff` unknown unit (9) — #533;
array/set type-guards `$size`/`$arrayElemAt`/`$in`/`$indexOfArray`/`$setUnion`/
`$setIntersection`/`$setDifference`/`$setIsSubset`/`$anyElementTrue`/`$allElementsTrue`/
`$mergeObjects`/`$range` (17124/28689/40081/40090/17043/17047/17048/17046/17041/17040/
40400/34443) and string/binary type-guards `$regexMatch`/`$regexFind`/`$regexFindAll`
(51104)/`$indexOfBytes` (40091/40092)/`$binarySize` (51276)/`$bsonSize` (31393) — with
`$arrayElemAt`+`$in`+the `$regex*` ops being **silent accepts** fixed on both engines — #535.
`$trim`/`$ltrim`/`$rtrim` (50699) and `$split` (40085/40086/40087) already matched
(#515/#500). Date/misc type-guards `$dateToString`/`$dateToParts` non-date (16006), `$dateFromString`
non-string (241), `$dateAdd`/`$dateSubtract`/`$dateTrunc` bad unit (9), `$let` undefined
var (17276), `$switch` no-branches (40068), `$ifNull` 1-arg (1257300), `$getField`/
`$setField` non-string field (5654602/4161107), `$sortArray` bad sortBy (2942507),
`$convert` missing input/to (9), `$dateDiff` missing param (5166303/4/5) — with
`$dateToString`+`$dateDiff` being **silent accepts** fixed both engines — #536.
`$trim`/`$ltrim`/`$rtrim` (50699) and `$split` (40085/40086/40087) already matched
(#515/#500). **A discovery sweep (three passes, ~120 operator error cases) showed the
divergences were bounded (~dozens, not hundreds); the named type-guard rows are now all
cleared (#525–#536). Remaining: `$meta` bad arg (17308 — `$meta` isn't an expression
operator in SecantusDB, gives 168), `$sum`/`$avg`/`$max`/`$min` as *expression* operators
(a 5.0+ feature, currently 168), and `$strcasecmp` numeric coercion (sec is stricter than
mongod) — small feature-gaps, not error-code nits. The rest is per-message text only.**

## Minor / out-of-scope
- `$where` unsupported (SecantusDB rejects; mongod runs JS — intentionally out of scope).
- `$meta:"indexKey"` projection drops field; `$near` legacy without a geo index matches
  (mongod ERR 291); `$currentDate` timestamp ordinal (`,1` vs `,0`); `$sortByCount`
  count-tie ordering. All trivial.
