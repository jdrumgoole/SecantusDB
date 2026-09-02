### A fifth sweep: a dropped column DEFAULT, and enums compared by spelling

28 of 36 shapes matched PostgreSQL 14.13 across arrays, enums, domains, ranges
and DDL. Two of the misses were silently wrong rather than refused.

**`ALTER TABLE t ADD COLUMN c text DEFAULT 'z'` dropped the DEFAULT on the
floor.** Existing rows kept NULL — PostgreSQL backfills — and, worse, a *later*
insert that omitted the column got NULL too, because the default was never
recorded in the catalog at all. `NOT NULL DEFAULT 7` therefore left a NOT NULL
column holding NULL.

**An enum comparison answered by spelling.** An enum's order is its declared
label order, so `'happy' > 'ok'` is true for `mood AS ENUM ('sad','ok','happy')`
and false as text — and `WHERE m > 'ok'` returned `sad`. Sorting already knew
the declared order (`enum_orders`); comparison did not.

#### Fixed

- `ADD COLUMN … DEFAULT` records the default and backfills existing rows. A
  non-literal default is evaluated once, as PostgreSQL does.
- Range comparisons on an enum column in `WHERE` (and so in `UPDATE` /
  `DELETE`), both operand orders. An enum has a finite label set, so a range
  comparison is exactly a set membership — no ordinal needed at query time.
- `array_positions()`, and result types for `array_fill()` (an array of its
  value's type, not text) and `range_merge()` over `::int4range` **casts** —
  only the range *constructor* was recognised as a range operand.
- `version()` nested in an expression. It worked as a bare projection through
  the session-function path but reported `function current_version() is not
  supported` — sqlglot's node name, not one the user wrote — the moment it was
  nested.

#### Still divergent

`enum_range()` / `enum_first()` / `enum_last()`, and an enum comparison in the
SELECT *list* (`SELECT m > 'ok'`) rather than a WHERE — both need the catalog
inside the scalar evaluator.
