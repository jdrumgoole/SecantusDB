### PREPARE no longer evaluates a constant expression

A statement that is only parse-analysed no longer folds its constants. `PREPARE s as select 1/0` is prepared, and the division by zero is raised by `EXECUTE s`, as PostgreSQL does; a protocol-level Parse + Describe of `select 1/0` describes an `int4` column, and `CREATE TABLE AS select 1/0 ... WITH NO DATA` creates its table. EXPLAIN and a populating CREATE TABLE AS still plan the statement, and raise the error there, as PostgreSQL's `eval_const_expressions` does.

#### Fixed

- Rust PG server: PREPARE, a statement Describe and `CREATE TABLE AS ... WITH NO DATA` no longer evaluate a constant expression while describing it.
  Fixes RP-511 (tests/backlog/test_rp_found_rp500.py::test_prepare_analyses_without_evaluating)
