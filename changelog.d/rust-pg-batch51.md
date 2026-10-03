### Rust PostgreSQL server: shared row locks, FOR UPDATE through joins and views, streaming cursors in a block

#### Fixed

- `FOR SHARE` / `FOR KEY SHARE` took no lock. They are now shared row locks following PostgreSQL's conflict table: sharers coexist, writers and `FOR UPDATE` wait for them, and two sharers that both upgrade get 40P01.
- `FOR UPDATE` / `FOR SHARE` over a join, FROM-subquery or view locked nothing. It now locks the base rows behind the returned rows and honours `OF`.
- PostgreSQL's refusals of row locking are added: with DISTINCT, GROUP BY, aggregates, windows, UNION, or on the nullable side of an outer join.
- `ROLLBACK TO SAVEPOINT` releases later rows under REPEATABLE READ, after DDL, and for lock-only rows. When it does rewrite, a commit by another session no longer makes it fail.

#### Performance

- Inside a transaction block, an extended-protocol portal or a `DECLARE CURSOR` (with or without HOLD) over a plain one-table SELECT now streams in batches. Server memory for a 200 MB result grew 16-24 MB instead of 670-900 MB.
