### Rust PG server: OVER on a plain function is refused as PostgreSQL refuses it

A window call whose function is neither a window function nor an
aggregate now follows `ParseFuncOrColumn`: the call is resolved first, then
refused 42809, where it was this server's 0A000 gap message.

#### Fixed

- `select octet_length('x') over ()` is 42809 `OVER specified, but
  octet_length is not a window function nor an aggregate function`, for any
  built-in or user-defined plain function, at the call's position; it was
  0A000 `window function octet_length() is not supported yet`. The name is
  printed as written (`pg_catalog.upper`).
- The call's arguments and the function resolve first, so `upper(1) over
  ()` and `nosuch(1) over ()` are 42883 `function ... does not exist`, as
  without OVER, and the first aggregate-only clause is named in
  PostgreSQL's order: `now(*) over ()`, `upper(distinct 'a') over ()`,
  `upper('a' order by 1) over ()` and `upper('a') filter (where true) over
  ()` each name their clause. The refused call is never evaluated
  (`nextval(...) over ()` leaves the sequence where it was).
- An aggregate PostgreSQL has that this server does not yet run over a
  window (`stddev`, `variance`, ...) stays 0A000, now told apart by
  PostgreSQL 15.19's list of aggregates and window functions.
- Fixes RP-682 (tests/backlog/test_rp_found_rp671.py::test_over_on_a_plain_function_is_42809),
  pinned with tests/backlog/test_rp_found_rp671.py::test_over_on_a_plain_function_resolves_the_call_first,
  tests/backlog/test_rp_found_rp671.py::test_over_on_a_plain_function_calls_nothing
  and `over_on_a_plain_function_is_42809_after_the_call_resolves` in
  `secantus-pgplan`.
- Filed while working it: RP-868 (DISTINCT, ORDER BY or FILTER on a plain
  function call without OVER is accepted), RP-869 (an ordered-set
  aggregate under OVER is not refused as PostgreSQL refuses it) and RP-870
  (most built-in aggregates do not run over a window).
