### Rust PostgreSQL server: FOR UPDATE on tables without a primary key, faster nested subqueries

#### Fixed

- `FOR UPDATE` through a subquery on a table with no primary key locks only the rows behind the result.
- `sum` / `avg` of double precision returned Infinity on overflow. They now raise 22003, as PostgreSQL does, for plain aggregates and window functions.
- A user error inside a subquery reached the client as XX000 "could not read a subquery". It now keeps its own SQLSTATE.
- The psycopg gauge runner restores default SIGINT handling. A run launched in the background inherited SIGINT ignored, so psycopg's `test_ctrl_c` could never send its cancel.

#### Performance

2,000 × 2,000 rows, release build:

| shape | before | after | PostgreSQL 15 |
| --- | --- | --- | --- |
| nested correlated EXISTS | 1.23 s | 0.147 s | 0.128 s |
| grouping in a FROM subquery | 0.32 s | 0.045 s | 0.128 s |
| `sum` / `avg` of bigint, numeric or float under a filter | ~1.24 s | 0.03-0.07 s | ~0.146 s |
