### The Rust PostgreSQL server groups numerics by value

`select v, count(*) from t group by v` split a `numeric` column by its stored
form rather than its value, so `1.5` and `1.50` were different groups — and so
were `1e40`, `1e40.0` and `1e40.00`. Seven rows came back where PostgreSQL
returns three, with each group's `sum` wrong to match. Grouping compared the
BSON values structurally, and a numeric carries its display scale.

A grouping key is now reduced to the value PostgreSQL groups on, while the
group keeps its first row for display — the text PostgreSQL prints. Integers,
strings and every other type keep their own equality.

#### Fixed

- `crates/secantus-pgserver`: `group_key_ident` normalises a numeric grouping
  key; GROUP BY matches on it.

#### Testing

- `tests/test_rust_pgserver_slice.py`: grouping over scale variants and a wide
  value, with the counts, sums and printed texts PostgreSQL 14.24 returns.
- That file's server binary is now found on Windows (`.exe`), where all 1,194
  of its tests silently skipped.
