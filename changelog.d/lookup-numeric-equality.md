### `$lookup` joins numbers by value, and SQL set operations dedup by value

A `$lookup` with `localField` / `foreignField` returned no match for
`Decimal128("1.5")` against `Decimal128("1.500")`, or for `2` against
`Decimal128("2.0")`. On the Rust server it also missed `2` against `2.0`. The
Python server joined `true` to `1`. mongod 8.2.11 joins all of those pairs by
value except `true`/`1`. This only happened when the foreign field had no
index: that path is a hash join, and it keyed on the raw value. Python's
`Decimal128` hashes by representation, and the Rust server compared with
structural `Bson` equality. With an index, the query goes through the query
engine and was already right.

The PostgreSQL server lowers a join to `$lookup`, so the same bug meant
`select ... from t join u on t.v = u.v` returned no rows for numerics that
differed only in scale, or for a numeric against an int. Separately,
UNION / INTERSECT / EXCEPT, recursive-CTE dedup, evaluated DISTINCT, DISTINCT ON
and window PARTITION BY built each row's identity from `repr()`. So `1.5` and
`1.50` were different rows, INTERSECT of the pair was empty, and `0.0` / `-0.0`
split too.

#### Fixed

- `aggregate.py`: the `$lookup` hash join keys on the `$group` bucket key
  (numerics by value, one NaN, bool apart from numbers).
- `crates/secantus-commands`: `lookup_match` compares with the canonical BSON
  order instead of `==`.
- `sql/numeric.py`: `eq_key`, a by-value row identity, used by the set
  operations, DISTINCT, DISTINCT ON and PARTITION BY.

#### Testing

- `tests/test_lookup_numeric_equality.py`: both servers, with and without an
  index on the foreign field.
- `tests/test_mongod_differential.py`: `lookup-numeric-equality-by-value`.
- `tests/test_pg_numeric_value_equality.py`: joins, set operations, signed
  zero, NaN, DISTINCT ON and PARTITION BY.
