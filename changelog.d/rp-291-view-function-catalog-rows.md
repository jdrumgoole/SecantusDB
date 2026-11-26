### The Rust PG server reads a new view's and a new function's catalog rows from where PostgreSQL puts them

`pg_statio_sys_tables` counts the heap blocks behind the catalog rows a
CREATE VIEW or CREATE FUNCTION wrote, as PostgreSQL 15 does, and those rows
take the space in each database's catalog heaps that they take there.

#### Fixed

- A catalog lookup that returned a row CREATE VIEW or CREATE FUNCTION wrote
  counted no heap block. A view's first use in a new session read 3
  `pg_class`, 3 `pg_attribute` and no `pg_rewrite` blocks where PostgreSQL
  reads 6, 4 and 1, and a function's first call 1 `pg_proc` block where
  PostgreSQL reads 3. CREATE VIEW's row type and array type, its `pg_class`
  row and columns, its `_RETURN` rule's `pg_rewrite` row and the HOT update
  `relhasrules` makes are now placed in PostgreSQL's order, and so is
  CREATE FUNCTION's `pg_proc` row. A DROP of either, or a CASCADE that
  takes a view, leaves them dead when it commits.
- Each row is placed at the length `heap_form_tuple` gives it. The rule's
  `ev_action` is compressed as `heap_toast_insert_or_update` compresses a
  row longer than 2032 bytes, through a port of PostgreSQL's pglz
  compressor, and a `pg_proc` row's length follows its argument names,
  modes, body and settings. Both were measured with pageinspect on
  PostgreSQL 15.20.
- The tenth cycle of a module's DDL in one database read 4 `pg_attribute`
  blocks where PostgreSQL reads 3: the unplaced view and function rows had
  moved where the table's rows went.

Fixes RP-291, in part
(tests/backlog/test_rp_catalog_reads.py::test_a_views_and_a_functions_rows_are_read_from_the_heap,
tests/backlog/test_rp_catalog_reads.py::test_the_tenth_cycles_rows_read_what_the_firsts_do).
RP-291's dead-space gap test now witnesses that gap directly: after 20
cycles of DDL, PostgreSQL's `pg_class` and `pg_type` are as long as after one
(tests/backlog/test_rp_catalog_reads.py::test_a_dead_rows_space_takes_new_rows).
