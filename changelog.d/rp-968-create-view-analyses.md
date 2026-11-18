### CREATE VIEW folds no constant

A view whose query divides by zero is created, as on PostgreSQL, whose
`DefineView` only parse-analyses the query: the error is raised when the
view is read. CREATE VIEW raised it at definition.

#### Fixed

- CREATE VIEW describes its query without folding constants, so
  `create view v as select 1/0 as x` succeeds and `select * from v` raises
  22012.
  Fixes RP-968 (tests/backlog/test_rp_found_rp500.py::test_create_view_analyses_without_evaluating)
