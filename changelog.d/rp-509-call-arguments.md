### Rust PG server: CALL fills defaults, takes named arguments and picks overloads as PostgreSQL does

A `CALL` matched a procedure by its full parameter count alone. So `call
pd(1)` over `pd(a int, b int default 2)` was 42883, any named argument (`call
pd(b => 3, a => 1)`) was refused with 0A000 `NamedArgExpr is not supported
yet`, and a column reference among the arguments was 0A000 where PostgreSQL
gives 42703. Two overloads the arguments could not tell apart ran whichever
came first, where PostgreSQL gives 42725. A CALL now finds its procedure the
way PostgreSQL 15 does. `FuncnameGetCandidates` (with `include_out_arguments`)
and `MatchNamedCall` fill the parameters from the arguments: positional ones
first, then named ones, then a DEFAULT for each trailing parameter left out.
Then `func_get_detail` picks a candidate by the arguments' types (an exact
match, an implicit coercion, or `func_select_candidate`'s narrowing). A
function picked this way is still 42809.

#### Fixed

- `CALL` with a defaulted parameter left out evaluates the default at the
  call, and an untyped literal argument (`call pd('5')`) resolves.
- `CALL` takes named arguments (`=>` and `:=`), with PostgreSQL's 42601 for
  a name given twice or a positional argument after a named one, and its
  42883 signature (`procedure pd(b => integer) does not exist`) when nothing
  matches.
- A column reference in a CALL's arguments is 42703 or 42P01 at its token,
  as in a FROM-less SELECT.
- Overloads the arguments cannot separate are 42725 `procedure ... is not
  unique`, at the procedure's name. This includes one overload that matches
  only through its defaults.
- A PL/pgSQL `CALL` writes an OUT or INOUT value back through a named
  argument (`call p(x => v)`). The interpreter no longer binds the
  argument's name as a variable (`call po(r => r, ...)` was a syntax error).
- A CALL in a SQL function's body is checked at CREATE by the same lookup,
  so a named argument that matches nothing, or an ambiguous call, fails at
  CREATE as it does on PostgreSQL.

Fixes RP-509 (tests/backlog/test_rp_found_rp500.py::test_call_arguments),
pinned by
tests/test_rust_pgserver_slice.py::test_call_resolves_defaults_named_arguments_and_overloads
and the `funcsig` crate test `select_routine_follows_func_get_detail`.
