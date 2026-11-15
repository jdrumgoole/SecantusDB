### An overlong varchar assignment names the assignment

A PL/pgSQL assignment such as `x := 'abcdef'::text` into a `varchar(3)`
variable fails with 22001, as before, and now carries the same CONTEXT
PostgreSQL 15 sends: a `PL/pgSQL assignment "x := 'abcdef'::text"` frame
above the `at assignment` line. The length coercion is part of the
assignment's query, so over a constant it folds while the query is
planned and is framed by it; over a variable it still carries only the
`at assignment` line. Array element and slice assignments follow the
same rule.

#### Fixed

- Rust PG server: an overlong `varchar(n)` value assigned from a constant
  expression in PL/pgSQL carries its `PL/pgSQL assignment` frame.
  Fixes RP-752 (tests/backlog/test_rp_found_rp712.py::test_overlong_varchar_assignment_is_framed_by_the_assignment)
