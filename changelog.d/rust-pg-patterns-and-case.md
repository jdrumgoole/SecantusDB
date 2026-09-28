### `LIKE`, `ILIKE`, the regex operators and `CASE` on the Rust PostgreSQL server

Four of the seventeen refusals the 2026-09-28 survey found, closed together.
0/17 → 14/17 on a differential corpus against PostgreSQL 14.13.

#### Added

- `LIKE` / `ILIKE` / `NOT LIKE` / `NOT ILIKE`, anchored as SQL requires, with
  `%` and `_` wildcards, every other character literal, and an explicit
  `ESCAPE` (which the parser folds into a `like_escape(p, e)` call rather than
  a third operand).
- The regex operators `~`, `~*`, `!~`, `!~*`.
- Both work as predicates and as values (`select a like 'a%'`).
- `CASE` in the searched and simple forms, with lazy branch evaluation, `NULL`
  falling through as not-true, and a missing `ELSE` yielding `NULL` — including
  in a FROM-less `SELECT`, which routed targets through a separate allow-list.

#### Fixed

- `n LIKE 'x'` over a non-text column is now `42883 operator does not exist:
  integer ~~ unknown`, as PostgreSQL has it. Lowering it to a regex anyway
  returned no rows, silently.

#### Known limitations

- `CASE` used directly as a bare `WHERE` predicate is still `0A000`: a CASE does
  not lower to an MQL filter, and `lower_where` has no arm for it.
- `ORDER BY` over an expression is unchanged, still `0A000`.
