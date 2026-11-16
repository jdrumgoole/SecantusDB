### A missing FROM table is reported before the column references that name it

PostgreSQL opens a query's FROM relations before it reads the select list or the WHERE clause, so `select fs.c from nosuch` fails with `relation "nosuch" does not exist`, pointing at `nosuch`. The Rust PostgreSQL server checked the column qualifiers first and answered `missing FROM-clause entry for table "fs"` instead. For `select nosuch.c from nosuch` it gave the right message but pointed at the column's qualifier rather than the FROM item. Both now match PostgreSQL 15.19.

#### Fixed

- A single-table SELECT whose FROM relation does not exist reports 42P01 `relation ... does not exist` before any qualified column reference is checked. The error's position skips a name that is followed by `.`, because there the name qualifies a column and is not the relation.
  Fixes RP-545 (tests/backlog/test_rp_found_rp527.py::test_missing_from_relation_reported_first)
