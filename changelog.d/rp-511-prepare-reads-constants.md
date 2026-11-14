### PREPARE reads a constant it does not evaluate

PostgreSQL's PREPARE parse-analyses its statement. Analysis runs each
untyped string constant's input function wherever a cast or an operator
gives the constant a type, whether or not the expression is ever
evaluated. So `prepare s as select case when false then 'x'::int end`
fails with `invalid input syntax for type integer: "x"` at the constant.
The Rust PostgreSQL server prepared such a statement and raised the error
only if EXECUTE reached the constant. PREPARE now reads those constants
the same way CREATE FUNCTION does.

#### Fixed

- Fixes part of RP-511 (tests/backlog/test_rp_found_rp500.py::test_prepare_reads_constants); PREPARE still evaluates a FROM-less constant expression.
