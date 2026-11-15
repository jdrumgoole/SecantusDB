### An aggregate with the wrong number of arguments does not exist

Calling an aggregate with an argument count none of its forms takes, such as
`stddev(v, v)` or `corr(v)`, now fails the way PostgreSQL fails it: 42883
`function stddev(integer, integer) does not exist`, naming each argument's
type, with or without OVER. It used to be a 0A000 "not supported yet".

#### Fixed

- Rust PG server: a wrong-arity aggregate call is 42883 naming its argument types.
  Fixes RP-904 (tests/backlog/test_rp_found_rp870.py::test_an_aggregate_with_the_wrong_arity_is_42883)
