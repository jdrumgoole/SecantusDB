### $limit and $skip validate their argument like mongod

The `$limit` and `$skip` stages coerced their argument naively (`int(spec)`), so a
range of invalid inputs silently produced wrong results instead of the error
mongod raises: `$limit: 0` returned nothing (mongod: "the limit must be
positive"), `$limit: -1` did a Python negative-slice, and a bool or fractional
double was quietly truncated/coerced. The Rust server had the mirror-image bug —
it *rejected a valid whole-number double* (`$limit: 2.0`) because its integer
coercion didn't accept doubles, while still coercing a bool to 1.

Both stages now match mongod: a whole-number double is accepted (coerced to the
count), and a bool, a fractional double, or a negative value is rejected — the
Python server with mongod's exact codes (`$limit` 5107201, `$skip` 5107200,
plus 15958 for a zero `$limit`), the Rust core deferring to `BadValue`. `$skip: 0`
stays valid. Three-way mongod 7.0.12-verified.

#### Fixed

- `$limit` / `$skip` accept a whole-number double and reject a bool / fractional /
  negative argument (and `$limit` a zero) with mongod's exact error code, instead
  of silently coercing it (Python) or rejecting a valid `2.0` (Rust).
