### An ungrouped whole row is named as PostgreSQL names it

A grouped query that reads a table's whole row outside an aggregate, without
grouping by that row, is an error in PostgreSQL, and the error names the row
itself: `select fs, count(*) from fs group by a` reports `column "fs.*" must
appear in the GROUP BY clause or be used in an aggregate function`, pointing
at the reference. The Rust PostgreSQL server named the row's first column
instead (`fs.a` or `fs.c`), with no position. It now names `fs.*` at the
reference, in the select list, ORDER BY and HAVING alike, and still accepts
the row when the query groups by the row or by the table's whole primary key.

#### Fixed

- An ungrouped whole-row reference's 42803 names `t.*` at the reference
  (`check_ungrouped_columns`).
  Fixes RP-536 (tests/backlog/test_rp_found_rp532.py::test_ungrouped_whole_row)
