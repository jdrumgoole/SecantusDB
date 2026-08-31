### Aggregation operators answered the wrong BSON type, and an integer overflow crashed the update path

A sweep of all 143 aggregation expression operators against a real mongod 8.2.11
found 42 cases where both servers returned a value and the value was wrong.
Nearly all of them were one of three rules the engines did not implement.

#### Fixed

- **The rounding operators are type-preserving.** `$ceil` / `$floor` / `$trunc`
  / `$round` answered a Python `int` for a double operand, so `{$ceil: 1.5}`
  returned `2` where mongod returns `2.0` — a different BSON type, which then
  compares and sorts differently downstream.
- **`long` is contagious through arithmetic.** `$add` / `$subtract` /
  `$multiply` / `$mod` / `$pow` / `$abs` and the four rounding operators all
  narrowed a 64-bit operand back to `int` when the result happened to fit in 32
  bits, so `Int64(1) + 1` answered an int where mongod answers a long. An int32
  result that outgrows its width now widens to long on its own, as mongod's
  does (`{$abs: -2147483648}`).
- **An integral result past int64 no longer fails the command.** It saturates
  to a double in an aggregation (`{$pow: [2, 64]}`), matching mongod. It
  previously reached `bson.encode` as an unbounded Python int, whose
  `OverflowError` surfaced to the client as `internal server error`.
- **`$inc` / `$mul` past int64 now fail the write with mongod's error** (code
  2, `Failed to apply $inc operations to current value ((NumberLong)…) for
  document {_id: …}`) rather than crashing. The same unencodable int was
  reaching `bson.encode` from *inside* the storage layer's update transaction.
- **`$mod` truncates toward zero.** It used Python's flooring `%`, which gives
  the wrong sign whenever an operand is negative — `{$mod: [-5, 2]}` answered
  `1` where mongod answers `-1`. Three of the four sign combinations were wrong.
- **`$toLower` / `$toUpper` coerce their operand to a string first.** They
  passed a non-string straight through, so `{$toLower: 1.5}` answered the
  number `1.5` rather than the string `"1.5"`. That conversion is deliberately
  *not* `$toString`'s: it accepts a javascript value but rejects a bool and an
  ObjectId (Location16007), renders a double with six significant digits where
  `$toString` round-trips it, and turns null and missing into `""` where
  `$toString` gives null.
- **`$toString` renders mongod's forms**, not Python's `str()`: `true` / `false`
  for a bool (it answered `True` / `False`), ISO-8601 for a date, base64 for
  binary, and `ConversionFailure` (241) for the types mongod refuses.
- **Doubles inside error messages** use mongod's six-significant-digit
  rendering, so `{$acos: 1099511627776}` names `1.09951e+12` rather than
  `1099511627776.0`.
- **`$degreesToRadians` / `$radiansToDegrees`** multiply by a single
  precomputed constant the way mongod does; computing `x * pi / 180` differed
  in the last bit. They also now report a non-numeric operand as Location28765
  like the rest of the math operators.
- **`$sin` / `$cos` / `$tan` / `$asin` / `$acos` / `$atan` / `$atan2` keep a
  Decimal128 operand's type.** `decimal` has no circular functions, so these
  fell through to the double path and narrowed a 34-digit operand to 17 digits.

After the change the Rust server has **no** value differences left across the
3,884-case sweep (was 33) and the Python server has five (was 42), all of them
a last-digit disagreement on a Decimal128 transcendental where mongod's own
answer is 1-2 ulp below the correctly-rounded value.
