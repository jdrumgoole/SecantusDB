### The Rust PostgreSQL server places an aggregate at the query level its columns belong to, and groups ROLLUP / CUBE from one read

In PostgreSQL an aggregate belongs to the lowest query level whose columns it
reads. So in `select (select max(s.x) from t) from s`, `max(s.x)` is the OUTER
query's aggregate: the outer query returns one row, and the subquery fails
with "more than one row returned" when `t` has several rows. The Rust
PostgreSQL server computed the aggregate once per outer row instead, and
returned a confident wrong answer. It now follows PostgreSQL's rule in select
lists, HAVING, ORDER BY, EXISTS / IN / ARRAY subqueries and nested subqueries.
Such an aggregate placed in WHERE, a JOIN condition, GROUP BY or a LATERAL
FROM item is now rejected with 42803, as in PostgreSQL.

GROUPING SETS / ROLLUP / CUBE over an input too big for memory read that input
once per grouping set (batch 66), so a ROLLUP over a join ran the join three
times. The input is now read once, and only the keys and aggregate inputs are
kept. A small input is read once too; batch 66 read it twice. Joins whose
right side spills to disk no longer re-sort the whole joined rows to restore
their order.

#### Fixed

- An aggregate inside a subquery that reads only outer columns is the outer
  query's aggregate (PostgreSQL's `agglevelsup`), at any depth. One misplaced
  in WHERE / JOIN ON / GROUP BY / a LATERAL subquery raises 42803.
- A correlated scalar subquery that returns an outer column
  (`(select s.x) from s`) reported its column as `text` instead of the
  column's type.
- A window function in a SELECT with no FROM (`select max(1) over ()`, or a
  subquery's `max(s.x) over ()`) was refused with 0A000.
- An unread LATERAL subquery output is not computed, so
  `(select a/b x ...) s, lateral (select s.x) l` answers where it raised
  22012.

#### Changed

- GROUPING SETS / ROLLUP / CUBE past the memory bound read their input once
  (unless the aggregate inputs are about as wide as the rows).
  On 300,000 rows of 2 KB (release build, 64 MB WiredTiger cache), a ROLLUP
  over a join went from 7.6 s to 2.2 s, and CUBE from 5.8 s to 1.7 s.
  Below the bound, the input is read once again.
- A grace hash join restores row order by sorting an index of the joined
  rows, not the rows themselves.
