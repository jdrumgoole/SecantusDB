### A built-in routine is looked up among its overloads

GRANT and COMMENT ON a built-in routine now find it as PostgreSQL's
`LookupFuncWithArgs` does, among its own `pg_proc` overloads. A bare
`grant execute on function lower to public` names three functions and is
42725 `function name "lower" is not unique`, with the hint to give the
argument list; `comment on routine abs` says `routine name`. A built-in is
a function, so `grant execute on procedure abs(int)` is 42809
`abs(integer) is not a procedure`, and a signature no overload has is the
missing function's 42883.

#### Fixed

- Rust PG server: a built-in routine named in GRANT / COMMENT ON is
  resolved among its overloads and its kind.
  Fixes RP-519 in part (tests/backlog/test_rp_found_rp514.py::test_builtin_routine_resolved_by_its_overloads);
  a built-in's comment stays open under RP-519.
