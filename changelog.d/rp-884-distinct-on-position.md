### Rust PG server: a DISTINCT ON position is checked against the select list

A DISTINCT ON item written as an integer is a position in the select list,
as PostgreSQL reads it. One outside the list -- `distinct on (9) a`,
`distinct on (0) a`, a position past what a `*` expands to -- is now
PostgreSQL's 42P10 `DISTINCT ON position N is not in select list` at the
number, instead of a key of one value that quietly returned the first row.
A position that falls inside a `*`, as in `select distinct on (2) * from t`,
now names the column the star stands for there instead of being refused.

#### Fixed

- The check sits beside RP-872's non-integer constant check
  (`secantus-pgplan/src/group_sql92.rs`) and finds the first refused item
  in item order, as `findTargetlistEntrySQL92` does: `distinct on (9, 1.5)`
  is the position's 42P10. An error in a clause PostgreSQL analyses
  earlier still comes first, including an ORDER BY error in a statement
  whose earlier DISTINCT ON items would fail the ORDER BY match.
- A position inside a `*` or `t.*` is rewritten to that FROM item's column.
  The DISTINCT ON / ORDER BY match check now expands a star too, so
  `select distinct on (2) * ... order by 1` (and `distinct on (b) * ...
  order by a`) is PostgreSQL's 42P10 `SELECT DISTINCT ON expressions must
  match initial ORDER BY expressions` instead of returning rows.
- A VALUES list now has known columns (`column1`, ... each of the domain
  every row's value is of, as `select_common_type` keeps it), so a star
  over one in FROM expands. The same knowledge reaches `CREATE TABLE AS`:
  `create table c as values (1::d)` now makes a column of the domain `d`
  and enforces its CHECK, where it made an `integer` column.
- A 42P10 `ORDER BY` / `GROUP BY` / `DISTINCT ON position N is not in
  select list` is located at the number inside its own clause: an earlier
  token equal to it (`distinct on (9) a ... order by 9`) no longer takes
  the position.
- Fixes RP-884 (tests/backlog/test_rp_found_rp872.py::test_a_distinct_on_position_is_checked_against_the_select_list,
  tests/backlog/test_rp_found_rp872.py::test_a_values_column_of_a_domain_is_of_the_domain),
  with a crate test (`secantus-pgplan`
  `a_distinct_on_position_is_checked_against_the_select_list`).
