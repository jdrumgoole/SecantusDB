### Rust PG server: a DISTINCT sorts on every column after its ORDER BY

A plain `SELECT DISTINCT` with an `ORDER BY` now sorts as PostgreSQL
does: on the `ORDER BY`, then on every select-list entry it does not
already hold, ascending (`transformDistinctClause`). The rows come back
in that order and `EXPLAIN` prints those keys.

#### Fixed

- `select distinct b, a+1 from e order by b` sorted on `b` alone, so
  within each `b` the rows came back in the order the DISTINCT kept them;
  they come back ordered by `a + 1` as well, and `EXPLAIN` prints `Sort
  Key: b, ((a + 1))` (it printed `b`). The same goes for `select distinct
  a, b ... order by b desc` (`b DESC, a`), `select distinct * ... order
  by b` (`b, a`), and a key given by output name or position. An entry
  listed twice, or already in the `ORDER BY`, is sorted on once.
- An `EXPLAIN` Sort Key dropped a NULLS placement that was not its
  direction's default: `order by b desc nulls last` printed `b DESC`, and
  `order by b nulls first` printed `b`. They print `b DESC NULLS LAST` and
  `b NULLS FIRST`.

Fixes RP-1126 (tests/backlog/test_rp_explain_costs.py::test_plan[a distinct sorted on a prefix sorts on every distinct column], tests/backlog/test_rp_found_rp1126.py::test_a_distinct_sorts_on_every_column_after_its_order_by). Fixes RP-1292 (tests/backlog/test_rp_explain_costs.py::test_plan[a sort key prints a nulls placement that is not the default]). Files RP-1293.
