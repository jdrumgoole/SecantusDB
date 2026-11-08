### A subquery reads an outer FROM-subquery's columns by their real names

The Rust PostgreSQL server now finds a correlated subquery's bare column
name in an outer FROM-subquery the way PostgreSQL does. `select (select a)
from (select 1 a union select 2) t` answers 1 and 2 instead of a 42703
`column "a" does not exist`, and so does a subquery reading a column the
FROM-subquery names through a `*`, an unnamed target such as `count(*)`, a
VALUES list's `column1`, or a column an alias list leaves unrenamed (`t(x)`
over two columns keeps the second one's name).

#### Fixed

- `secantus-pgplan`: the names an outer FROM-subquery offers a subquery
  (`collect_from_columns`) are the body's output names as its range table
  entry has them (`colhint::select_output_columns`, PostgreSQL's
  `transformRangeSubselect`): a set operation's leftmost leaf's, a star
  expanded, an unnamed target named by `FigureColname`, and an alias's
  column list renaming only the leading columns. They were read from the
  body's own target list, which a set operation leaves empty.
- Fixes RP-505 (tests/backlog/test_rp_fromless_analysis.py::test_correlation_over_a_set_operation,
  tests/backlog/test_rp_fromless_analysis.py::test_correlation_reads_a_from_subquerys_output_names).
- Filed RP-506: a WITH item in FROM, and a star over a function or a USING
  join, still offer no names.
