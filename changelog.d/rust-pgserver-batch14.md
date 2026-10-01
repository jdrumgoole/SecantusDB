### The Rust PostgreSQL server: transaction control in procedures, semi-joins, join pushdown, recursive views

Batch 14 adds transaction control to procedures and `DO` blocks, and plans
`EXISTS` and selective joins far faster. It also fixes three silent-data bugs.
Every change is measured against PostgreSQL 15.

#### Fixed

- **A failing `DO` block left its earlier writes committed.** A block run on
  the function interpreter wrote each statement on its own. Its statements
  now join the session's transaction.
- **A caught exception did not undo its block's writes.** A `BEGIN ...
  EXCEPTION` block now runs as a subtransaction, as PostgreSQL's does.
- **`COMMIT` in a SQL-language procedure crashed the connection.** It is now
  `0A000`, as on PostgreSQL.
- `VALUES` columns take the rows' common type. A string beside a numeric was
  summed as 0.
- NaN and -0 group as PostgreSQL groups them in `GROUP BY`, `DISTINCT`,
  `count(DISTINCT)` and window partitions.
- A `COLLATE` inside a `VALUES` list, subquery or CTE carries out to the
  derived column. `GROUP BY` under a nondeterministic collation answers the
  group's first value.
- An aggregate under a subscript, `(array_agg(x))[1]`, is an aggregate.
- A view over `SELECT * FROM (VALUES ...) v` had no columns.
- A table column-alias list, `FROM t r(a, b)`, answered 42703.

#### Added

- `COMMIT` / `ROLLBACK` [`AND CHAIN`] in procedures and `DO` blocks. They are
  allowed only outside a transaction block, as on PostgreSQL; anywhere else
  they are 2D000.
- `CALL` inside PL/pgSQL, writing `OUT` / `INOUT` values back to variables.
- `CREATE VIEW ... WITH RECURSIVE`, and recursive CTEs inside FROM subqueries.
- `pg_get_viewdef` prints PostgreSQL's form for more view shapes:
  - `VALUES`;
  - `ROWS` / `GROUPS` frames with `EXCLUDE`;
  - `LATERAL` and column-alias lists;
  - a set operation's `ORDER BY` / `LIMIT` / `OFFSET`;
  - `WITH RECURSIVE` and CTE column lists;
  - renamed relations (`t t_1`).

#### Performance

- A join applies each single-table WHERE condition to that table's own
  scan, so the scan can use an index. Only tables on the preserved side of
  an outer join are filtered early. A selective join over 20,000 rows went
  from 731 ms to 1.7 ms.
- A WHERE-level `EXISTS` / `NOT EXISTS` over one equality runs as the
  uncorrelated `IN` / null-safe `NOT IN` it equals. Over 2,000 x 2,000 rows,
  `EXISTS` went from 7.2 s to 46 ms.
- Window partitions are hashed rather than scanned.

New corpora: `values_common_type`, `derived_collation`,
`procedure_transactions`, `join_pushdown`, `semi_join` and `viewdef_shapes`.
All six are at 0 divergences against PostgreSQL 15.
