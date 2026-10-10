Title: secantus-pg 0.1.0-beta.8: a statement sent as text reuses its plan
Date: 2026-10-10 15:00:00
Slug: secantus-pg-0-1-0-beta-8
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.8 plans an unprepared statement once per shape, not once per literal: one row by primary key with a different value each time went from 111 to 43 microseconds on a Mac, against PostgreSQL 15's 40. It also hides the tables of `public` when `public` is off the `search_path`, and runs expressions over grouping-set keys in any clause.

The last release note ended with the place this server was furthest behind
PostgreSQL: a statement sent as text, with its values written into it and a
different value each time. This release is that piece of work.

## An unprepared statement reuses its plan

A prepared statement already reuses its plan. A statement sent as text was
looked up by its text, so `select t from x where k = 17` and
`select t from x where k = 18` were two statements, each parsed and planned
from nothing. Any client that writes its values into the SQL text sends
exactly that.

Three things changed.

- **The text is looked up with its literals taken out.** Integer and plain
  string literals are lifted from the text, and what is left names a shape.
  The second statement of a shape learns a plan with holes in it, and the
  third and every later one fills the holes with its own values. This covers
  `INSERT`, `UPDATE` and `DELETE` as well as `SELECT`.
- **A plan with holes is learned the second time, not the first.** Learning
  one costs two more plans, which was over half the time of a statement
  whose text never came again.
- **One command alone is not parsed twice.** It used to be parsed once to
  split it from its neighbours and again to be planned.

One row by primary key, sent as text, on a Mac with the two released
binaries and PostgreSQL 15.19, in microseconds a statement:

| | a different key each time | the same text each time |
| --- | --- | --- |
| 0.1.0-beta.7 | 111 | 41 |
| 0.1.0-beta.8 | 43 | 41 |
| PostgreSQL 15.19 | 40 | 28 |

A statement with a new key now costs what a repeated one does. PostgreSQL is
still ahead on both, and by more on the repeated text.

That is a Mac, with the client on the same machine. The figures we publish
on the server's page come from a Linux droplet, where the same statement
took 380 microseconds against PostgreSQL's 128 before this change. We have
not re-measured there yet, so the page still carries the earlier numbers and
says so.

A plan that is reused must give the answers a fresh plan gives. A corpus of
252 statements, where each shape runs twice before the values that matter
(a sign, a value too wide for `int4`, a quote inside a string, a string
where a number belongs, a value that fails a `CHECK`), agrees with
PostgreSQL 15.19 on every row and every column type.

What is not taken out, and is still planned every time: a number with a
decimal point or an exponent, a string with a backslash or an `E''` prefix,
and a negative number written against its operator (`k =-5`).

## `public` off the `search_path`

A table in `public` is stored under its bare name. So with `public` taken
off the `search_path`, a bare name went on finding it:

```sql
set search_path to pg_catalog;
insert into orders values (1);   -- PostgreSQL: relation "orders" does not exist
```

This server ran the `INSERT`. It answers 42P01 now, for reads and writes
alike. `public.orders` works as it did, and a view created over the table
keeps reading it. Two lookups still find the table where PostgreSQL does
not: a name passed as a string (`nextval('s')`, `'orders'::regclass`) and
`DROP TABLE IF EXISTS orders`, which drops it where PostgreSQL skips it.

## Grouping sets

Under `ROLLUP`, `CUBE` and `GROUPING SETS`, these were refused and now run:

```sql
select region || '/' || city, sum(n) from sales group by rollup (region, city);
select region, sum(n) from sales group by rollup (region) having region || 'x' = 'eux';
select case when grouping(region) = 1 then 'total' else region end, sum(n)
  from sales group by rollup (region);
select upper(region) || '!' from sales group by cube (upper(region));
```

`GROUP BY (a, b)` now groups by `a` and `b`. A corpus of 45 such statements
differs from PostgreSQL 15.19 on three: a parenthesised step inside a
`ROLLUP` (`rollup ((a, b), c)`) is still refused, a subquery in a grouped
query cannot name the outer table by its own name without an alias, and one
error names a column without its table.

## Smaller fixes

- A string literal compared with a `bytea` column matched no row at all. It
  is read as `bytea` now.
- An unterminated string, quoted identifier or comment reports where it
  opens (`select 'abc` is position 8). It had no position.

## psycopg's own tests

psycopg 3.3.4's unmodified suite, run on macOS against this release's code:
of the 5,731 tests that ran, 5,544 pass and none fail.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.8` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.8)
