### A non-boolean argument of AND, OR or NOT is reported at its first token

When an argument of `AND`, `OR` or `NOT` is not boolean, PostgreSQL reports the 42804 error at the argument's leftmost token: `select ('x' || 'y') and true` points at the `'x'`. The Rust PostgreSQL server pointed at the `||` or the `and`, and one character early under `NOT`. It now reports the same position PostgreSQL does, in queries and in constant-folded values such as an INSERT's `values (1 and true)`.

#### Fixed

- Rust PG server: the 42804 for a non-boolean argument of AND / OR / NOT carries the argument's leftmost-token position (`exprLocation`), as PostgreSQL 15.19 does.

Fixes RP-812 (tests/backlog/test_rp_found_rp808.py::test_a_connectives_argument_is_reported_at_its_leftmost_token)
