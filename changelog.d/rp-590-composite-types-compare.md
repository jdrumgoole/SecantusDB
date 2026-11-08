### Rust PG server: two different composite types compare as records

`select row(1)::ct = row(1)::ct2`, with `ct` and `ct2` two composite
types, failed with 42883 `operator does not exist: ct = ct2`. The check for
an enum compared with another type treated every composite as an enum,
because a composite is installed among the user types with no labels.
PostgreSQL 15.19 answers `t`. A composite coerces to `record`, and
`record_eq` / `record_cmp` compare the fields by position. They raise 42804
for two different field types, or for different widths once the shared
fields compare equal.

Once the operator resolved, a WHERE over a composite column gave a silently
wrong answer. The filter compared the stored record whole, its field types
and names included. `where v = row(1, 'a')::cd`, `cd` having `v`'s field
types under other names, matched no row where PostgreSQL matches one.
`where v = row(1, 2)::cb` and `where v in (row(1, 'a', 0)::cc)` returned no
rows where PostgreSQL raises 42804. So did `v = (1, 'a')`, a `ROW(...)` with
an `unknown` field, which was wrong before this change too.

#### Fixed

- `=`, `<>`, `<`, `<=`, `>` and `>=` between two different composite types
  now resolve to `record`'s operators. The `record_image` forms (`*=` ..
  `*>=`) pass the same check but are still 0A000 when they run (RP-650).
  Any other operator between them is still 42883 (`ct + ct2`). An enum
  beside a composite is still refused.
- In a WHERE, a composite column compared with a record of other field types
  or names is now evaluated row by row, through `record_eq` /
  `record_cmp`'s checks. This covers `=` and the ordering operators,
  IS [NOT] DISTINCT FROM, IN and NOT IN, `= ANY` / `<> ALL`, BETWEEN and
  NOT. UPDATE and DELETE use the same WHERE path. A record of the column's
  own type is still a filter.

Fixes RP-590 (tests/backlog/test_rp_found_rp530.py::test_two_composite_types_compare,
tests/backlog/test_rp_found_rp590.py::test_where_compares_a_composite_column_as_a_record).
The crate regression tests are `secantus-pgplan`'s
`tests::two_composite_types_compare_as_records` and
`tests::composite_column_against_another_record_type_is_per_row`.

#### Filed

- RP-650: the `record_image` operators (`*=` .. `*>=`) are refused with
  0A000. On PostgreSQL `(select row(1.0)) *= (select row(1.00))` is `f`.
