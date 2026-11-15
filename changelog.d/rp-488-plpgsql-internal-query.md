### A PL/pgSQL expression's analysis error points into the expression

When an expression in a PL/pgSQL body named something that does not exist
-- `raise notice '%', nosuch.x`, `x := nosuch.x + 1`, `return nosuch.x` --
the Rust PostgreSQL server sent the error with no QUERY and no internal
position, and when the body ran as a function it pointed the error at the
calling statement instead. Like PostgreSQL's `_SPI_error_callback`, it now
moves the error's cursor position into the expression as written, so psql
prints `QUERY:` and the `LINE 1:` caret under the name at fault.

#### Fixed

- A class-42 (or 3F000) error raised analysing a PL/pgSQL expression
  carries the expression as its internal query and the position in it, in
  a DO block and in a function call alike, and no statement position.
  Fixes RP-488 (tests/backlog/test_rp_plpgsql_labels.py::test_expression_error_carries_its_query,
  tests/backlog/test_rp_plpgsql_labels.py::test_function_expression_error_carries_its_query)
