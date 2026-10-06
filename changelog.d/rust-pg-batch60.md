### Rust PostgreSQL server: jsonb compares by value, ungrouped aggregates in bounded memory

`jsonb` values were compared by their stored text, which keeps each
number's scale, so `{"x": 1}` and `{"x": 1.0}` came out as two different
values. PostgreSQL compares jsonb numbers as numerics. The wrong answers
were not limited to DISTINCT and GROUP BY: `WHERE j = '{"x":1.0}'`
returned no rows at all, even for a row stored with exactly that text. An
aggregate with no GROUP BY now runs in bounded memory.

#### Fixed

- jsonb equality is by value everywhere it is decided: WHERE `=` / `<>` /
  `IN` / `= ANY` / `IS [NOT] DISTINCT FROM`, joins, IN and EXISTS
  semi-joins, SELECT DISTINCT, DISTINCT ON, GROUP BY (including its
  sorted spill), `count(DISTINCT j)`, UNION / INTERSECT / EXCEPT, a
  window's PARTITION BY, and UNIQUE / PRIMARY KEY constraints on INSERT
  and UPDATE. Checked against PostgreSQL 15.19 with corpus `b60_jsonb_eq`.
- An UPDATE now enforces a UNIQUE constraint over a column with a
  nondeterministic collation. Before, only INSERT checked it.
- `SELECT DISTINCT count(*) ... GROUP BY k ORDER BY 1` (or `ORDER BY` the
  aggregate's alias) returned an error. It now returns the distinct
  aggregate values in order.

#### Changed

- An aggregate with no GROUP BY over one stored table (with no filter or a
  collection-scan filter) reads its input one chunk at a time and combines
  the partial results. This covers `count`, `min`, `max`, `bool_and`,
  `bool_or`, and an exact integer or numeric `sum`. On 300,000 rows of 2 KB
  the server's memory growth fell from 1755 MB to 58 MB.
- An equality on a `jsonb` column is now checked row by row, so it no
  longer uses a btree index on that column. A jsonb UNIQUE constraint reads
  the whole table once per write statement.
