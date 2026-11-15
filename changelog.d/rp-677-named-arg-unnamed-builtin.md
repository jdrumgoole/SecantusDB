### A named argument to a built-in without argument names is 42883

A built-in such as `nextval` declares no argument names, so PostgreSQL finds
no function for `nextval(regclass := 'seq')` and says so: 42883 `function
nextval(regclass => unknown) does not exist`, at the call, with the
no-function-matches hint. The Rust PostgreSQL server answered 0A000
`NamedArgExpr is not supported yet` instead, a gap where PostgreSQL gives a
definite answer. It now gives PostgreSQL's.

#### Fixed

- Rust PG server: a named argument to a built-in whose parameters have no
  names is 42883 naming the call, each named argument printed `name => type`.
  Fixes RP-677 (tests/backlog/test_rp_found_rp458.py::test_named_argument_to_a_builtin_without_argument_names)
