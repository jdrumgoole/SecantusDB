### Rust PG server: a set operation's type mismatch under `INSERT ... SELECT` is positioned

`insert into t(c) select (1,'x')::ctp union select row(2,'y')` now reports
42804 `UNION types ctp and record cannot be matched` at the right arm's
column, as PostgreSQL 15 does, where it had no error position. The same
holds for `INTERSECT` and `EXCEPT`, a parenthesised query, and a `WITH`
before the `INSERT` or the query.

#### Fixed

- The error position of a set operation's unmatched column types is found
  in the query an `INSERT ... SELECT` writes, as in a plain query.

Fixes RP-1050 (tests/backlog/test_rp_found_rp552.py::test_union_composite_record_position)
