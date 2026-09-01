### `$convert` and the `$toX` operators now agree with MongoDB on 480 shapes

`$toInt: " 5 "` returned `5`. Python's `int()` strips surrounding whitespace and
accepts PEP-515 underscores, so `"1_0"` became `10` — MongoDB's parser accepts
neither, and both are wrong *values*, not wrong messages. `$toDate: 1` returned
an epoch date where MongoDB refuses an int32 outright, and `$convert` of an
empty string to `bool` returned `false` where every BSON string is true. And
`Decimal128("Infinity")` to an integer target reached `int(Decimal("Infinity"))`,
whose `OverflowError` escaped the handler and reached the client as
`1 internal server error`.

Underneath all of it was the same shape: two implementations of one conversion.
`$toInt` / `$toLong` / `$toDouble` / `$toDecimal` each carried their own copy of
the logic alongside `$convert`'s, and the copies had drifted — different
overflow messages, different unsupported-type errors, and none of them knew
MongoDB separates NaN from infinity from merely out of range. The shorthands now
delegate to `$convert`, so they cannot drift again.

String parsing is strict and reports MongoDB's reason for refusing: `No digits`,
`Did not consume whole string.`, `Overflow`, `Leading whitespace`, `Empty
string`, `Did not consume any digits`, `Failed to parse string to decimal` —
which are not the same set for integers, doubles and decimals, and are not
interchangeable. Hexadecimal input gets MongoDB's other message shape entirely.
`$toObjectId`, which did not exist, now does. And the conversion shorthands
accept the single-element array form (`{$toInt: ["$field"]}` — the way a field
reference is naturally written) with MongoDB's own wrong-arity error for
anything else.

A sweep of 480 `$convert` and `$toX` shapes plus 51 date and objectId shapes
against MongoDB 8.2.11 is now at zero divergences.

#### Fixed

- `expressions.py`: strict string→number parsing with MongoDB's per-target
  reasons; NaN / infinity / overflow separated; unsupported source types answer
  `ConversionFailure` naming both ends; `$toString` uses BSON spellings
  (`true`, `Infinity`, `NaN`); every string converts to `true`.
- `expressions.py`: `to: "date"` accepts a long / double / decimal / ObjectId /
  Timestamp and rejects an int32, and returns a naive UTC datetime that compares
  with a stored date instead of raising `TypeError` against one.
- `expressions.py`: the four `$toX` numeric shorthands delegate to `$convert`.

#### Added

- `expressions.py`: the `$toObjectId` operator, which did not exist.
- `expressions.py`: `$stdDevPop` and `$stdDevSamp` in EXPRESSION position — over
  an array argument in `$project` / `$addFields`. The accumulator forms shipped
  long ago; the expression forms answered `Unknown expression`. They share the
  accumulator's fold, so the two forms cannot disagree. Found by the same sweep,
  which also confirmed that most of the "missing operators" list in
  `docs/feature-comparison.md` had quietly started working.

#### Fixed (Rust engine, to keep the two in step)

- `secantus-core`: `$convert: {to: "bool"}` returns true for every string. The
  `$toBool` shorthand already did — the two copies had drifted *inside one
  engine*, the same shape as the Python split this change collapses.
- `secantus-core`: `to: "date"` refuses an int32, matching MongoDB; it used to
  read one as epoch milliseconds.
