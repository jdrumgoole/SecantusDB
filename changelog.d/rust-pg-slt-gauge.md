### Rust PostgreSQL server: the sqllogictest gauge, and what it found

The sqllogictest gauge can now run against the Rust PG server (`SECANTUS_GAUGE_SERVER=rust`). Its report goes to a separate `.validation/slt-raw-rust-server.json`, and it refuses a binary built from a different source tree. The first run passed 28 of 60 lane-files. These fixes came from it, each checked against PostgreSQL 15.

#### Fixed

- `BETWEEN` in every shape: `NULL BETWEEN …`, `NOT x BETWEEN …`, expression bounds and `BETWEEN SYMMETRIC`. These answered `0A000 this operator form is not supported yet`. `x NOT BETWEEN NULL AND hi` now keeps the rows above `hi` (it matched nothing).
- `SELECT DISTINCT` over an aggregate or a grouped expression (it was `0A000`). `SELECT *` / `t.*` with `GROUP BY` is fixed too (it was a 42803 naming an empty column).
- A correlated subquery whose inner FROM aliases the outer table's own name (`FROM t1 AS x WHERE x.b < t1.b`) failed with `42P01`. It now reads the outer row, as in PostgreSQL.
- An aggregate inside `CASE`, `COALESCE`, `NULLIF` or an `IN (...)` list is now computed. It was left out, and the expression answered NULL.
- Arithmetic over a numeric aggregate (`- avg(x)`, `avg(x) / count(*)`) failed with `42883 operator does not exist: integer - text`. It now works.
- A scalar subquery column takes its inner column's name.
- The "must appear in the GROUP BY clause" error names the column qualified, as PostgreSQL does (`tab2.col1`).
