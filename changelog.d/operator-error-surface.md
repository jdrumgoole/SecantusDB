### Query and update operators answer mongod's errors — and `$bits*` answers the right documents

A systematic sweep crossed every query and update operator with every
pathological argument — 2,226 shapes — against mongod 8.2.11. 583 disagreed. A
hand-picked sample of 32 had found only 12 of those, and had missed `$bits*`
entirely; `$bits*` turned out to be returning the wrong **documents**, not the
wrong message.

#### Fixed

- **`$bitsAllSet` / `$bitsAnySet` / `$bitsAllClear` / `$bitsAnyClear` took a
  plain integer and nothing else.** They silently skipped array elements (which
  mongod matches element-wise, like every other multikey operator), doubles,
  `Decimal128`, and BinData values — and rejected BinData masks outright. A
  document holding `[1, 4]` or `5.0` was invisible to a query that should match
  it. 35 of 44 probed shapes were wrong; now none.
- **Two arguments crashed with `internal server error`.**
  `{v: {$regex: BinData}}` compiled as a *bytes* regex and then raised on a
  string subject, and `{v: {$type: Code(...)}}` hit an unhashable-key set test.
  Both now report mongod's error.
- **`{v: {$not: {a: 1}}}` matched.** An ordinary field name inside `$not`
  degraded to an equality test which `$not` then negated, returning the
  document; mongod refuses the query. Every key inside `$not` must be an
  operator.
- **`$rename` applied when its target was a `bson.Code`**, because `Code`
  subclasses `str` and passed the type check.
- **`{v: {$type: []}}` answered an empty result set** where mongod refuses an
  empty alias list, and `Decimal128` is a valid numeric type code.
- **`$currentDate` reported the wrong problem** for an unrecognized option key:
  mongod names the key before it looks at `$type`, so `{$type: "date", a: 1}`
  names `a`.
- **A whole `Decimal128` is a valid `$pop` direction, `$size` and `$bits*`
  mask.** All three rejected it.

#### Changed

- The value renderer mongod uses to echo an offending argument had drifted into
  **five** partial copies. `$inc` / `$mul` / `$pop` / `$rename` all used the
  scalar-only one, so an array printed `[1]` where mongod prints `[ 1 ]` and a
  sub-document printed `{'a': 1}` where mongod prints `{ a: 1 }`. There is now
  one, in `bsontypes`, alongside the type-name helper that had already been
  consolidated for the same reason.
- `$pop`, `$size` and the `$bits*` mask share mongod's numeric-argument
  ladder — NaN, out of range, fractional and non-integral `Decimal128` each get
  their own sentence — extracted as one helper.
- Message wording now matches mongod throughout: `$and argument must be an
  array`, `unknown operator: $x`, `malformed mod, needs to be an array` (which
  mongod distinguishes from a too-short array), and the `$bit` family's four
  distinct texts.

`tools/probes/operator_error_surface.py` is the standing cover.
