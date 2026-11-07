### Rust PG server: a function's parameter can be qualified by the function's name

A function body may name its parameter `p` as `fname.p`, the way PostgreSQL
lets a SQL or PL/pgSQL function qualify a parameter by the function's own
name, at any depth of subquery: `select fq1.p` in `fq1(p int)` answers the
argument. The Rust PostgreSQL server refused every such reference, as
`column "fq1" does not exist` or `missing FROM-clause entry for table
"fq2"`. It now binds them, and keeps PostgreSQL's precedence: when a
relation of the statement known by the function's name has a column `p`,
the column is read, not the parameter.

#### Fixed

- `secantus-pg`: a SQL function body's `fname.p` is bound as its parameter
  `p` unless a relation known as `fname` may have the column -- a stored
  table's columns are read from the catalog, a CTE's or FROM subquery's
  figured from its aliases or target list (`FigureColname`, now public in
  `secantus-pgplan`). A PL/pgSQL expression's `fname.x` reads a variable
  of the block the function's name labels: a parameter (IN or OUT), FOUND
  or a trigger's TG_ variable, even one an inner declaration shadows.
  Fixes RP-451 (tests/backlog/test_rp_subquery_scope.py::test_function_qualified_parameter);
  the precedence rules are pinned by the `function_parameter_tests` crate
  tests.
- Filed while working it, each with a gap test in
  `tests/backlog/test_rp_function_bodies.py`: RP-461 (a bare parameter
  name is replaced wherever it is a word, ahead of a column and inside an
  output alias), RP-462 (a composite parameter's field), RP-463 (a PL/pgSQL
  block label does not qualify its variables), RP-464 (a SQL function
  ending in a write's RETURNING returns NULL) and RP-465 (CREATE FUNCTION
  does not check a SQL body).
