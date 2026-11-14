### A column of no collation is not grouped, made distinct or compared

A VALUES or set-operation column whose rows carry conflicting implicit
collations has no collation, and PostgreSQL refuses to hash or compare it:
`GROUP BY` it is 42P22 `could not determine which collation to use for
string hashing`, and `SELECT DISTINCT`, `count(DISTINCT ...)` and a
comparison of two such values in the select list are 42P22 `... for string
comparison`. The Rust PostgreSQL server used to answer each of them
bytewise; it now fails as PostgreSQL does, and a sort by such a column
fails as it starts, over no rows too. `WHERE v = v` still answers, as the
planner reduces it to `v IS NOT NULL`.

#### Fixed

- GROUP BY, DISTINCT, count(DISTINCT) and comparisons over a column of no
  collation are 42P22; a sort by one fails over no rows too.
  Fixes RP-820 (tests/backlog/test_rp_found_rp796.py::test_a_column_of_no_collation_is_not_hashed_or_compared)
