### A DO block's syntax error is placed in its body, found before it runs

PostgreSQL compiles a whole DO block before any of it runs, parsing each
expression as it goes, and places a syntax error at the token in the body
where the scanner stopped. The Rust PostgreSQL server placed a DO block's
syntax error at the end of the statement, and for the blocks it runs on its
own short path found it only when the statement ran, under that statement's
CONTEXT. Every DO block is now compiled first, its error placed where
PostgreSQL places it; an error without a scanner position, such as an unknown
condition name, keeps the `compilation of PL/pgSQL function
"inline_code_block" near line N` context and no position.

#### Fixed

- `do $$ declare x int := 1 +; begin null; end $$` and `do $$ begin perform
  1 +; end $$` are 42601 at the position PostgreSQL 15 reports.
- A DO block's compile error without a position carries the compile
  context on every path.

Fixes RP-958 (tests/backlog/test_rp_found_rp619.py::test_expression_syntax_error_is_placed_in_the_body,
tests/backlog/test_rp_found_rp619.py::test_do_block_compile_error_has_context)

Files RP-1040 (a CREATE FUNCTION's compile error without a position has no
compile context).
