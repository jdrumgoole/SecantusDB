### `numeric` is exact again — `0.1 + 0.2 = 0.3` is true

Decimal literals were read as floats, so Postgres' arbitrary-precision exact
`numeric` behaved like a double. `0.1 + 0.2 = 0.3` answered false,
`SELECT 0.000000` came back as `0` with its scale discarded, and a value wider
than a double silently dropped digits — `12345678901234567890.12345 + 1`
returned `1.2345678901234567E+19`, which for money-shaped data is corruption
rather than rounding.

A literal is now the same exact decimal a `numeric` column already stored, so
values written, computed and read back all agree. Integers are unaffected, and
so is integer division.

Comparisons involving a decimal were wrong in a quieter way: the operators
could not compare a decimal against an int or a float at all, and answered
false instead. Any predicate mixing the two — a column against a decimal
expression, a stored `numeric` against a literal — silently matched nothing.

#### Added

- `typemap.number_literal`, the single mapping from a numeric literal to its
  Postgres type. The planner and the scalar evaluator carried separate copies
  of this, which is why an earlier attempt at this fix left arithmetic on
  floats.
- `typemap.unwrap_numeric` / `typemap.negate` / `typemap.to_decimal128`.

#### Fixed

- Decimal literals are exact and keep their scale, so `numeric` arithmetic no
  longer inherits floating-point error or loses digits.
- Comparison operators handle decimals instead of silently answering false.
