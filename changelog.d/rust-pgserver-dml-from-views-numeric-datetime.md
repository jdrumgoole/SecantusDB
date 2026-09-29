### The Rust PostgreSQL server: UPDATE ... FROM, updatable views, exact numeric math, and PostgreSQL's full date/time input

`UPDATE ... FROM` and `DELETE ... USING` used to ignore their extra FROM and
write every row the statement's own WHERE allowed. They now touch only the
joined rows. `RETURNING` may read the joined item, and an ambiguous unqualified
column is `42702`, as in PostgreSQL.

A view over a single table is now automatically updatable. `INSERT` /
`UPDATE` / `DELETE` through it are rewritten onto the base table, a computed
view column is read-only, and `WITH [LOCAL | CASCADED] CHECK OPTION` refuses a
row the view would not show (`44000`). A `READ ONLY` transaction, or
`default_transaction_read_only`, now refuses every write, `nextval()`
included (`25006`); before, it wrote.

Integer arithmetic overflows as PostgreSQL does (`22003 integer out of range`).
Before, an `int4` result silently widened to `int8`. The numeric
transcendentals (`sqrt`, `exp`, `ln`, `log`, `power`, `^`) are exact, at
PostgreSQL's result scale, instead of float approximations.

Date/time input now goes through a transcription of PostgreSQL's
`DecodeDateTime`. `Jan 5, 2020`, `5 January 2020 10:30 PM`, `1/5/2020` (in the
session's DateStyle order), `20200105T103000`, `J2458854`, `y2020m01d05`, zone
names and abbreviations, and `today` / `tomorrow` all read as they do on
PostgreSQL. `AT TIME ZONE` is implemented.

#### Added

- `bit(n)` / `bit varying(n)`: literals, casts to and from integers and text,
  `& | # ~ << >> ||`, `get_bit` / `set_bit` / `bit_count` / `length` /
  `position` / `substring` / `overlay`, binary wire format; the integer bitwise
  operators.
- `AT TIME ZONE` / `timezone()`, with named zones, abbreviations, POSIX
  offsets and intervals.
- `scale()`, `min_scale()`, `trim_scale()`, `numeric_send()`, and
  `numeric(p,s)` rounding and overflow (`22003`) on casts and assignment.
- Expression indexes, `CREATE [UNIQUE] INDEX ... (lower(email))`: a unique
  one is enforced on INSERT and UPDATE. Index keys may declare `NULLS FIRST` /
  `LAST`.
- `ORDER BY` over an aggregate result (`ORDER BY count(*) DESC`, by alias or
  position); `GROUPING()`; an aggregate over a WHERE that does not lower to a
  filter (`WHERE lower(email) = ...`).
- `generate_series` over `numeric`, and over `date` / `timestamp` /
  `timestamptz` with an interval step.
- `EXPLAIN (FORMAT YAML | XML)`, and `Parent Relationship` / `Alias` /
  `Parallel Aware` in the structured formats.
- `SET (a, b) = (1, 2)`, subqueries in `INSERT ... RETURNING`,
  `has_*_privilege()`, `to_regclass()`, `IS [NOT] TRUE / FALSE / UNKNOWN`, and
  the `"char"` type.

#### Fixed

- `UPDATE ... RETURNING` / `DELETE ... RETURNING` with bound parameters sent
  rows without a RowDescription, which a client rejects.
- A `float4` is rounded to single precision and prints as `float4out` does
  (`0.33333334`).
- `CASE` / `COALESCE` / `greatest` / `least` answer the branches' common type;
  `real + int` is `double precision`; a negative `LIMIT` / `OFFSET` is
  `2201W` / `2201X`.
- `upper` / `lower` / `initcap` use the simple case mapping (`upper('ß')` is
  `ß`); `lc_collate` / `lc_ctype` report `C.UTF-8`.
- A DST-ambiguous or skipped local time resolves to the offset PostgreSQL picks.
- The information_schema views report `varchar` / `name` / `"char"` column
  types, and `column_default` shows a folded default as written (`(1 + 2)`).
- A wide-year or BC `timestamptz` prints with its zone offset.
