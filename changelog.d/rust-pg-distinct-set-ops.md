### The Rust PostgreSQL server learns DISTINCT and the set operations

`SELECT DISTINCT` was ignored: `select distinct s from t` returned the
duplicates, and `DISTINCT ON (...)` was dropped the same way. `UNION`,
`INTERSECT` and `EXCEPT` were not recognised at all — a set operation has no
FROM clause of its own, so it fell through to the constant planner and
answered a single empty row. Both returned wrong results without an error.

All of them now work, matching PostgreSQL 14.24 across 33 differential cases:
NULLs group as one value, `1.5` and `1.50` are one row, `DISTINCT ON` keeps
the first row per key in ORDER BY order, and `ALL` keeps multiplicities —
`INTERSECT ALL` pairs each right-hand row with one left-hand row and `EXCEPT
ALL` subtracts them.

A set operation's columns take their names from the left side and their types
from both: within a type category PostgreSQL widens (`int4` with `int8` is
`bigint`, `numeric` with `float8` is `double precision`), across categories it
refuses, and this now says the same thing in the same words —
`UNION types text and integer cannot be matched` (42804). A count mismatch is
42601, and an untyped NULL takes the other side's type.

#### Added

- `crates/secantus-pgplan`: `Distinct` on a planned select, and `SetOpSelect`
  as a statement of its own, with the ORDER BY / LIMIT / OFFSET that belong to
  the combined result.
- `crates/secantus-pgserver`: dedup for DISTINCT and DISTINCT ON, and
  `set_op_rows` — both matching rows by value, so numerics that differ only in
  scale count once.

#### Testing

- `tests/test_rust_pgserver_slice.py`: DISTINCT, DISTINCT ON, every set
  operation with and without `ALL`, ordering and limits, and the type rules,
  with values measured against PostgreSQL 14.24.
