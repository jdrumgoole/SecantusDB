### Rust PG server: HAVING reads `IS DISTINCT FROM`, `= ANY` and `<> ALL`, and compares `char(n)` as `bpchar`

`having max(k) is distinct from 2` now keeps the groups whose maximum is not
2, as on PostgreSQL 15.19; it had kept only the one that is, because the
HAVING planner's comparison shortcut read the operator name and not the
expression's kind, so `IS DISTINCT FROM` ran as `=`, `IS DISTINCT FROM NULL`
as `= NULL`, and `= ANY ('{1,2}')` / `<> ALL (...)` compared against the
array's text. The shortcut now takes only a plain operator, and leaves every
other kind to the grouped query's split, which plans it as a typed WHERE
over the groups. A comparison over a `char(n)` grouped key or `min` / `max`
goes the same way, so `having max(f) = 'xy  '`, `= 'xy'::char(3)` and `f =
'xy  '` over a `char(3)` column drop the blanks as `bpchar`'s operators do.

#### Fixed

- HAVING's `IS [NOT] DISTINCT FROM`, `= ANY (...)` and `<> ALL (...)` over a
  grouped value answer PostgreSQL's rows instead of a plain `=` / `<>`'s.
- A HAVING comparison of a `char(n)` grouped key or `min` / `max` compares
  as `bpchar`.

Fixes RP-680 (tests/backlog/test_rp_found_rp671.py::test_having_over_char_n_min_max_compares_as_bpchar,
tests/backlog/test_rp_found_rp671.py::test_having_reads_each_expression_kind)

Files RP-685 (a `char(n)` column compared with a `text` value compares as
`bpchar`) and RP-684 (`= ANY` of a `char(n)` array finds nothing over a
`char(n)` column).
