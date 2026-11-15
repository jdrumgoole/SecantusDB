### A PL/pgSQL json literal's input error names its assignment

An assignment such as `x := 'zz'` to a `jsonb` variable now reports its
input error the way PostgreSQL does: the assignment as the internal query,
the position of the literal in it, and no extra `PL/pgSQL assignment`
frame. The json message quotes no value, so the literal is found from the
`JSON data, line N:` context line instead.

#### Fixed

- Fixes RP-753 (tests/backlog/test_rp_found_rp712.py::test_json_literal_input_error_in_an_assignment_carries_its_query)
