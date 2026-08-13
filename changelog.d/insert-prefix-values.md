### INSERT rows can fill a column prefix, like Postgres

`INSERT INTO t VALUES (1, 2)` into a three-column table is legal
PostgreSQL — the row fills a prefix of the columns and the rest take
their defaults. The SQL server required an exact arity match, which broke
pgjdbc's rewritten batch inserts (`reWriteBatchedInserts=true` collapses a
repeated INSERT into one multi-VALUES statement without a column list)
and several JDBC tests that insert partial rows. Too many expressions is
still an error, and an explicit column list still requires an exact
match, both with PostgreSQL's wording. pgjdbc's
BatchedInsertReWriteEnabledTest (60) and TimeTest now pass in full.

#### Fixed
- A VALUES row shorter than the table's column list (no explicit column
  list) fills the leading columns; remaining columns take DEFAULT/NULL.
