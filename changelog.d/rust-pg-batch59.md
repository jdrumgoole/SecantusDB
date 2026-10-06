### The Rust PostgreSQL server: hashed DISTINCT, bounded-memory DISTINCT, expression sorts and large GROUP BYs, and cheaper primary-key reads

`DISTINCT`, `GROUP BY`, `INTERSECT` and `EXCEPT` no longer compare each row
against every distinct row kept so far. That comparison was quadratic, and a
`SELECT DISTINCT` over 300,000 distinct rows did not finish. Rows now go
through a hash on the same value identity. `DISTINCT`, `DISTINCT ON` and an
`ORDER BY` over an expression now stream in bounded memory, as an `ORDER BY`
of stored columns already did. So does a `GROUP BY` whose input is larger
than 64 MB. A primary-key read is about 3 µs faster.

#### Changed

- Rust PostgreSQL server: `SELECT DISTINCT`, `DISTINCT ON`, `GROUP BY`
  (grouping sets included), `INTERSECT [ALL]`, `EXCEPT [ALL]` and
  `agg(DISTINCT ...)` use a hash on the value identity. Equality is still
  decided by `==` inside a bucket, so the answers are unchanged
  (`distinct_set.rs`).
- Rust PostgreSQL server: a `DECLARE CURSOR`, a block's portal and an
  extended-protocol portal outside a block now stream with `DISTINCT`,
  `DISTINCT ON` or an `ORDER BY` over an expression. Each one sorts in
  bounded memory and keeps the first of each run of equal rows. Fetching
  300,000 rows of 2 KB 1,000 at a time after a restart (release build), the
  server's RSS growth went down as follows:
  - expression `ORDER BY`, outside a block: 1,719 → 40 MB;
  - expression `ORDER BY`, named cursor: 2,384 → 44 MB;
  - `DISTINCT`: 1,371 → 33 MB;
  - `DISTINCT ON`: 1,372 → 37 MB.
- Rust PostgreSQL server: a plain `GROUP BY` over one stored table whose
  input passes 64 MB sorts on the group key and aggregates one group at a
  time. This also works inside a transaction block. RSS growth for the
  300,000-row table went from 1,430 MB to 106–110 MB. Smaller inputs are
  grouped as before. `SECANTUS_PG_GROUP_MEMORY_BYTES` lowers the threshold
  for tests.
- Rust PostgreSQL server: a primary-key read no longer copies the role list,
  the session user, the role and the temp-schema name on every statement. It
  also no longer resolves its relations through the search path on every
  statement. Release build, `bench43.py`, two interleaved runs: 47.4 / 49.0 →
  45.4 / 45.3 µs. PostgreSQL 15.19 takes 33.1 µs.

#### Fixed

- Rust PostgreSQL server: `DISTINCT` / `GROUP BY` over an array compares its
  elements by value. `{1.0}` and `{1.00}` are now one numeric array, and
  `{NaN}` equals itself in a float array. A set operation now matches an
  integer literal against a numeric column by value, so `select n ... union
  select 1` no longer returns `1` twice. These are PostgreSQL 15.19's
  answers (corpus `b59_distinct`).
