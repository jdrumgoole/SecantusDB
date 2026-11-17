### An outer aggregate in a RETURNING subquery is refused

An aggregate inside a RETURNING subquery whose arguments are only the
written row's columns -- `update t set a = 1 returning (select
array_agg(t.b) from u)` -- belongs to the write itself, as PostgreSQL's
`check_agg_arguments` assigns it, and the Rust PostgreSQL server now
refuses it with 42803 `aggregate functions are not allowed in RETURNING`
for UPDATE (including over its FROM items), DELETE and INSERT, instead of
answering a value per row or 21000.

#### Fixed

- Fixes RP-944 (tests/backlog/test_rp_found_rp664.py::test_an_outer_aggregate_in_a_returning_subquery_is_refused)
