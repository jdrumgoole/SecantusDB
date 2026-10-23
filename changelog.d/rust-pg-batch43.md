### Rust PostgreSQL server: a user function named like a built-in no longer hides it

A user function named after a built-in (`create function public.lower(int)`)
is now one candidate beside the built-in, as on PostgreSQL 15.19, rather
than a replacement for every call of the name.

#### Fixed

- Fixes RP-173
  (`tests/backlog/test_rp_batch36.py::test_scenario[a user function of a built-in's name]`,
  `tests/backlog/test_rp_batch43.py::test_scenario[a user function of a built-in's name over columns and the path]`).
  With `public.lower(int)` defined, `lower('Q')` and `pg_catalog.lower('Q')`
  called the user function, and `abs(-3)` called a user `public.abs(int)`.
  - **How a call is resolved.** Each bare call of a built-in's name is
    resolved against both candidates, as `func_select_candidate` would.
    The candidate with more exact matches wins, then the one that takes an
    unknown literal as a string, then the earlier schema on the search path
    (`pg_catalog` unless the path puts the user's schema first).
  - **Argument types** come from the statement's literals, casts and the
    columns of the tables it reads.
  - **Qualified calls.** `pg_catalog.lower(5)` no longer reaches the user
    function either, and is PostgreSQL's 42883.
