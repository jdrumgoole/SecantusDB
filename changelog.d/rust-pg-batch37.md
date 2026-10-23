### Rust PostgreSQL server: a view's row type, and attnums that survive DROP COLUMN

#### Fixed

- Fixes RP-117 (`tests/backlog/test_rp_batch26.py::test_scenario[a view's row type is a type]`
  and `tests/backlog/test_rp_batch37.py::test_scenario[a view's row type follows the view]`):
  a view's row type was listed in `pg_type` but was no type, so `'v'::regtype` was 42704
  and `row(1)::v` was refused, and a whole-row reference to the view was `record`.
  - The view's record now keeps its output columns' names and types, from
    `CREATE [OR REPLACE] VIEW` and every rename.
  - The planner registers that as a composite, as a table's row type is:
    `v` and `v[]` resolve, a value casts to it, and `pg_typeof(v)` over the
    view is `v`.
- Fixes RP-157 (`tests/backlog/test_rp_batch32.py::test_scenario[a dropped table column leaves its attnum]`
  and `tests/backlog/test_rp_batch37.py::test_scenario[a dropped column's slot keeps every attnum]`):
  DROP COLUMN renumbered the columns after it. Each column now keeps the
  attnum it was given, and a dropped one leaves PostgreSQL's
  `........pg.dropped.N........` slot in `pg_attribute`. An added column
  takes the next number. The same attnums are now used by:
  - `information_schema.columns.ordinal_position`;
  - `pg_index.indkey`, `pg_attrdef.adnum`, `pg_constraint`'s `conkey` and
    `confkey`, `pg_trigger.tgattr`, `pg_statistic.staattnum`, `pg_depend`;
  - column comments;
  - the RowDescription's `ftablecol`.
- Fixes RP-175 (`tests/backlog/test_rp_batch37.py::test_scenario[a subquery under a view's alias is no view]`):
  the record of which FROM subqueries had been views outlived its
  statement. A later subquery under an alias an earlier view had was
  taken for that view (its `tableoid`, and now its row type). The record
  is now each statement's own.
