### A FROM alias that shares a PL/pgSQL record's name stays an alias

In a PL/pgSQL block with `declare r t689`, `(select r.a from s689.st r)`
now reads `r.a` as the record's field and leaves the alias `r` alone, as
PostgreSQL does: it substitutes a variable only at a column reference, and
an alias is none. The binder replaced the alias token with the record's
value, so the subquery was a syntax error.

#### Fixed

- `Interp::bind` finds each aliased relation's alias token from the
  statement's parse tree and never binds a variable there.
- Fixes RP-982 (tests/backlog/test_rp_found_rp689.py::test_a_from_alias_a_record_variable_shares_is_not_the_variable)
