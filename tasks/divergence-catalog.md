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
  input to int32 like mongod. Both engines. (`$toLong` is still unimplemented — a
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
- **Array/string type guards**: `$concat:["a",5]`→"a5" (mongod 16702); non-array to
  `$reverseArray`/`$concatArrays`/`$map`/`$filter`/`$reduce`/`$first`/`$last`/`$slice`
  → `None` (mongod 34435/…); `$trim`/`$ltrim`/`$rtrim {chars:5}` → unchanged (50700);
  `$indexOfBytes`/`$indexOfCP` whole-double start ignored (→-1) and bool start coerced
  (mongod 40096); `$toDate` on int/bool accepts (mongod 241). (expressions)
- **Date-arg whole-double / bool**: `$dateAdd`/`$dateSubtract {amount:2.0}` and
  `$dateTrunc {binSize:2.0}` over-reject valid whole doubles; `amount:true`/`binSize:true`
  compute (mongod 5166405/5439017). `$toLong: 2.7` rejects valid (mongod truncates→2).
- **Query operator validation**: `$in`/`$nin` non-array + `$`-doc element FIXED (#494,
  → 2); `$regex` bad `$options` / `$options`-without-`$regex` / non-string `$regex`
  FIXED (#496, → 51108 / 2); `$elemMatch` non-doc (query) + `$not` invalid arg
  FIXED (#497, → 2); `$all` non-array + mixed/non-`$elemMatch` `$`-doc FIXED (#499,
  → 2). Remaining: `$elemMatch` non-doc in *projection* (proj→31274). (query.py)
- ~~**Projection `$slice`**~~ **FIXED (#506):** non-number scalar / short / long
  array → 28667; 2/3-elem array not `[skip, positive-limit]` → 28724. Both engines
  (Rust core defers). (projection.py)
- **`$type` validation**: unknown alias / fractional code / bool / bad code silently
  no-match (mongod: unknown-alias 2, bad-code 2 [special code-0 msg], bool 14); Rust
  also rejects valid whole-double codes. Valid alias set = 22 incl. deprecated; valid
  code set = {-1,1..19,127}. **Ground-truth captured.** (query.py / .rs)
- **Update `$push $sort`** bad spec (int/`{x:2}`) sorts anyway (mongod 2); scalar+doc-spec
  and mixed scalar/doc sort order wrong. `$pull`/`$pullAll` on a non-array field no-op
  (mongod 2). `arrayFilters` unused (mongod 9) / bad identifier (2) / empty (9) accepted.
  `$currentDate: {d:false}` rejected (mongod OK). (update.py)
- **Stages**: `$bucket` unsorted/mixed/dup boundaries, default-in-range, missing groupBy
  (40194/40193/40199/40198) accepted; `$bucketAuto {buckets:2.0}` rejects valid,
  `{buckets:true}`/`granularity:'BOGUS'` accepted (40241/40257); `$densify`
  `{step:true}`/`bounds:'partial'`/`[0]`/`[5,0]`/unit-on-numeric FIXED (#504, →
  14/5946802/5733403/5733402/6053600; `{step:1.5}` is mongod-valid, now computes);
  `$unwind {includeArrayIndex:5/true/'$i'}`, bare `path:'a'` (no `$`), non-bool
  `preserveNullAndEmptyArrays` accepted (28810/28818/28809/28822); `$sortByCount`
  number/non-expr-doc/no-`$` accepted (40149/40147/40148); `$facet` empty/non-array/
  non-object-stage/nested FIXED (#505, → 40169/40170/40171/40600); `$count`
  ''/dotted/`$`-prefixed/`_id` accepted (40157/40160/40158/15948); `$sort`
  bad-ordering/empty/`{v:true}` FIXED (#502, → 15975/15976/15974); `$project {}`
  empty accepted (51272). (aggregate.py)

## Tier 3 — wrong error code (both error; SecantusDB returns 14 or None vs a Location code)

Large, low-value count. Python raises but with code 14 (TypeMismatch) or None where
mongod uses a specific Location code — the Rust server already renders BadValue for
all of these (the standing error-code gap). Examples: `$strLenBytes`/`$strLenCP`
(34473/34471), `$split` (40085/40086/40087), `$trim` (50699), `$dateDiff` bad unit (9),
`$zip` (34461/34468), `$arrayToObject`/`$objectToArray` (40386/…), `$sortArray` non-array
(2942504), `$toInt`/`$toDouble`/`$toDecimal`/`$toDate` non-numeric string (241),
`$toLong` string (241), `$convert to:bad` (2), `$replaceOne`/`$replaceAll` (51746/51744),
plus the many update/stage missing-code (None) rows. **Recommend: defer as a batch, or
skip — the numeric CODE rarely gates a driver test and the Rust gap makes it moot there.**

## Minor / out-of-scope
- `$where` unsupported (SecantusDB rejects; mongod runs JS — intentionally out of scope).
- `$meta:"indexKey"` projection drops field; `$near` legacy without a geo index matches
  (mongod ERR 291); `$currentDate` timestamp ordinal (`,1` vs `,0`); `$sortByCount`
  count-tie ordering. All trivial.
