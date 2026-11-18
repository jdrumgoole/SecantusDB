### A function's CONTEXT label is fixed at the session's first compile

PostgreSQL labels a PL/pgSQL function once, when the session first
compiles it, and keeps that label while the compiled function is cached:
`s.f()` called first with `s` off the search path stays `PL/pgSQL function
s.f()` in CONTEXT after `set search_path = s`. The label now follows the
first call's path the same way, instead of each call's.

#### Fixed

- `secantusd-pg`: the session keeps each PL/pgSQL function's CONTEXT label
  from its first compile. An identical re-CREATE still keeps the old label,
  where PostgreSQL recompiles; that remainder is filed as RP-1051.
  Fixes RP-1034 (tests/backlog/test_rp_found_rp833.py::test_a_function_label_is_fixed_at_its_first_compile)
