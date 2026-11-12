### Rust PG server: an aggregate clause on a plain function is refused

DISTINCT, ORDER BY, FILTER, WITHIN GROUP or `(*)` on a call to a plain
function is now refused as PostgreSQL's `ParseFuncOrColumn` refuses it,
42809, after the call resolves. The planner used to read past these
clauses and answer the plain call, so `select upper(distinct 'a')` returned
`A` where PostgreSQL reports an error.

#### Fixed

- `select upper(distinct 'a')`, `select upper('a' order by 1)` and `select
  upper('a') filter (where true)` are each 42809 `<clause> specified, but
  upper is not an aggregate function`, at the call's position, in a select
  list, a WHERE, an ORDER BY or a subquery, for a built-in or a user
  function; `now(*)` is `now(*) specified, ...`. The first clause is named
  in PostgreSQL's order and the name is printed as written
  (`pg_catalog.upper`). Aggregates are unchanged.
- The function resolves first, so a call no function takes is still 42883:
  `upper(distinct 1)` is `function upper(integer) does not exist`.
  WITHIN GROUP's items count as arguments, as in PostgreSQL:
  `upper('a') within group (order by 1)` is 42883 `function upper(unknown,
  integer) does not exist` (it answered `A`), and `left('a') within group
  (order by 1)` is 42809.
- Fixes RP-868 (tests/backlog/test_rp_found_rp682.py::test_an_aggregate_clause_on_a_plain_function_is_42809),
  pinned with `an_aggregate_clause_on_a_plain_function_is_42809` in
  `secantus-pgplan`.
