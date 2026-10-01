### Rust PostgreSQL server: correlated subqueries with comparisons, and subquery column names

#### Changed

- The once-per-statement path for correlated subqueries now also takes comparisons (`<`, `<=`, `>`, `>=`, `<>`) against the outer row, for numbers and timestamps. `EXISTS (... t.x = o.a AND t.y > o.b)` at 2,000 × 2,000 rows went from 7.6 s to 40 ms (debug build), with the same answers as PostgreSQL.

#### Fixed

- A scalar subquery's column is named after its inner column, as in PostgreSQL. `EXISTS` and `ARRAY(...)` are named `exists` and `array`. Before, all three were `?column?`.
- With the names fixed, `ORDER BY id` over two output columns named `id` answers PostgreSQL's `42702 ORDER BY "id" is ambiguous`. It used to sort by one of them silently.
