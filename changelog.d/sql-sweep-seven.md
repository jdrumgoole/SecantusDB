### SQL: `char(n)` comparison, `WITH ORDINALITY` aliases, and `greatest`/`least` typing

A seventh differential sweep against PostgreSQL 14.13 — this one over `char(n)`
semantics, `INSERT … ON CONFLICT`, `GROUPING`/`ROLLUP`/`CUBE`, CTEs, row
locking and set operations — scored 20 of 24 and turned up four defects, one of
them a silently wrong answer.

#### Fixed

- `char(n)` columns now compare **blank-insensitively**, as Postgres does.
  `bpchar` comparison strips trailing spaces from *both* operands, so a
  `char(5)` holding `'ab'` matches `= 'ab'` **and** `= 'ab   '`. SecantusDB
  stores the value unpadded, which got the stored side right for free but left
  the literal side padded — so the second form quietly answered false where
  Postgres answers true, in `WHERE`, in a projection, and in `IN`. `varchar`
  is genuinely blank-sensitive and is deliberately left alone.
- `UNNEST(…) WITH ORDINALITY AS t(v, i)` now names its ordinality column `i`.
  sqlglot hoists an `UNNEST`'s last alias column into a separate `offset` slot
  rather than leaving it in the alias list, so the column fell back to the
  default name `ordinality` and `SELECT i` failed with
  `42703 column "i" does not exist`.
- `greatest()` / `least()` now report their arguments' type rather than `text`.
  `greatest(NULL, 1)` was sent as the *string* `'1'` under oid 25 where
  Postgres sends the integer `1`.

#### Added

- `num_nonnulls()` and `num_nulls()`.
