### Exact decimal numbers on the Rust PostgreSQL server

PostgreSQL's `numeric` type is for numbers that must be exact — money,
quantities, anything where a rounding error is a bug rather than a rounding
error. It is not a floating-point type, and the difference shows in two ways
that matter to a client: `1.50` and `1.5` are different values, and reading one
back gives a decimal rather than a float.

The Rust PostgreSQL server had been refusing decimals rather than storing them
as floats, on the grounds that returning the right magnitude under the wrong
type is worse than declining — the same reasoning that had a cast integer
arriving at a client as text earlier in this work. That refusal can now be
lifted: decimals are stored in a format that keeps the exact digits and the
trailing zeros, and are reported as the type they are.

There is a limit. PostgreSQL's decimals have no fixed size; the storage format
here holds 34 significant digits. A number needing more is refused rather than
quietly rounded, because a silently shortened number is indistinguishable from
a correct one.

#### Added

- `numeric` as a column type, a cast target, and the type of a decimal literal,
  keeping exact digits and trailing zeros (`1.50` stays `1.50`).

#### Fixed

- A decimal literal such as `1.5` was reported as a floating-point number.
  PostgreSQL reports it as `numeric`, and clients read that to decide whether
  they get an exact decimal or a float.
