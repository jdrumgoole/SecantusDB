### The wide-numeric oracle tests stop colliding in CI

`tests/test_pg_numeric_wide_grouping.py` compares against a live PostgreSQL,
and every xdist worker shares that one server. Each test dropped and recreated
a table named `t`, so two parametrisations raced each other and CI saw both
halves: `relation "t" does not exist` in one worker and a duplicate key on
`pg_type_typname_nsp_index` in another.

Each test now works in a schema of its own, which leaves the cases' SQL (which
names a bare `t`) unchanged and drops the schema afterwards.

#### Fixed

- `tests/test_pg_numeric_wide_grouping.py`: a per-test schema on the oracle
  side.
