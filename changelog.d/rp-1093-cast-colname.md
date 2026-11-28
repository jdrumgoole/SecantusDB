### A keyword function or a `COLLATE` under a cast keeps its column name

The Rust PostgreSQL server names an unaliased select-list column with its
port of PostgreSQL's `FigureColname`, so the shapes it used to miss are
named as PostgreSQL 15 names them.

#### Fixed

- `select user::text, current_date::text` named its columns `text`,
  `text`; they are `user`, `current_date`, as on PostgreSQL (a keyword
  function names its column, and a cast keeps that name).
- `select c.relname collate "C" from pg_class c` named its column
  `?column?`, and `(c.relname collate "C")::text` named it `text`; both
  are `relname` (a `COLLATE` is named for its argument).
- Fixes RP-1093 (`tests/backlog/test_rp_found_rp1046.py::test_a_cast_of_a_sql_value_function_keeps_its_name`,
  `test_a_collate_clause_keeps_its_argument_name`).
