### A DO block takes PL/pgSQL's `#` compile options

A DO block may open with the same `#variable_conflict` and
`#print_strict_params` lines a function body can, because PostgreSQL
compiles an inline block with the function grammar. The Rust PostgreSQL
server refused the `#` as a syntax error; such a block now runs on the
PL/pgSQL function interpreter, which reads those options.

#### Fixed

- A DO block whose body opens with a `#` compile option runs instead of
  failing with 42601 at the `#`.

Fixes RP-959 (tests/backlog/test_rp_found_rp619.py::test_do_block_takes_compile_options)
