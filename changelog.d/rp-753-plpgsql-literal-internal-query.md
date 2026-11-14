### A PL/pgSQL literal's input error points into its expression

When a literal in a PL/pgSQL expression cannot be read as its type --
`x := 'abc'` for an integer variable, `raise notice '%', 'abc'::int` --
the Rust PostgreSQL server now sends the expression's text as the error's
internal query and the literal's place in it as the internal position, as
PostgreSQL does, so psql prints the `QUERY:` line and the caret. Before, it
sent neither.

#### Fixed

- A literal input error in a PL/pgSQL expression carries the expression as
  its internal query and the literal's position in it. A `json` / `jsonb`
  literal, whose message quotes no value, is still open.
  Fixes part of RP-753 (tests/backlog/test_rp_found_rp712.py::test_literal_input_error_in_an_assignment_carries_its_query)
