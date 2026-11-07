### Rust PG server: a DO block with a label runs

A `DO` block whose body opens with a `<<label>>`, or labels a statement
inside it, ran on PostgreSQL and was a syntax error here.

#### Fixed

- `do $$ <<blk>> declare x int := 3; begin raise notice '%', blk.x; end $$`
  was 42601 `syntax error at or near "<"`, and a labelled loop inside a DO
  block (`begin <<l>> for i in 1..2 loop ... end loop; end`) 42601
  `syntax error at end of input`. The DO block's own path, which reads only
  `raise`, `execute`, `perform` and `null`, now hands a body with a label to
  the function interpreter, which reads labels (RP-463), as it already did
  for a DECLARE section or control flow.

Fixes RP-486 (tests/backlog/test_rp_plpgsql_labels.py::test_do_block_with_a_label)

#### Found while working it

- RP-492: a syntax error in a DO block the interpreter compiles carries no
  statement position (tests/backlog/test_rp_do_compile_errors.py).
