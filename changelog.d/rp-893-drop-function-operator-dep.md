### Rust PG server: DROP FUNCTION keeps an operator's function while the operator exists

Dropping the function behind a user operator used to succeed and leave the
operator in `pg_operator`, naming a function that no longer existed. In
PostgreSQL an operator depends on the function that implements it. A plain
DROP FUNCTION is therefore refused while the operator exists, and DROP
FUNCTION ... CASCADE drops the operator with it. This server now does the
same.

#### Fixed

- DROP FUNCTION refuses with 2BP01 `cannot drop function f(...) because
  other objects depend on it` while an operator is implemented by the
  function. The DETAIL names each operator as `getObjectDescription` does
  (`operator |=|(t,integer)`, with `NONE` for a prefix operator's missing
  left argument), in creation order, after any cast the function
  implements. CASCADE drops the operators, and its notice names or counts
  them.

Fixes RP-893 (tests/backlog/test_rp_found_rp546.py::test_drop_function_of_an_operator_is_refused,
tests/backlog/test_rp_found_rp546.py::test_drop_function_of_two_prefix_operators).
