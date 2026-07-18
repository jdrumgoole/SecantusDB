### Unary math operators reject non-numeric operands instead of coercing or crashing

`$abs`, `$ceil`, `$floor`, `$sqrt`, `$exp`, `$ln`, `$log10`, `$round`, and
`$trunc` never type-checked their operand. A string leaked a raw Python
`TypeError` (surfacing as a generic error, not a clean server error), and a bool
was silently coerced to `1` / `0` and computed on. mongod rejects both: a
non-numeric operand is `Location28765` (`$round` / `$trunc` use `51081`), while
`null` still passes through as `null`. Both servers now match.

The Python server carries mongod's exact codes; the Rust core defers these cases
to `BadValue` (so the Rust server rejects them too, rather than coercing a bool).
Whole-double operands and every valid numeric input are unaffected. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$abs` / `$ceil` / `$floor` / `$sqrt` / `$exp` / `$ln` / `$log10` reject a
  string or bool operand with `Location28765`, and `$round` / `$trunc` with
  `51081`, instead of coercing a bool to `1`/`0` or leaking a Python `TypeError`
  (both servers).
