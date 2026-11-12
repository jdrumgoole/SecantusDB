### Rust PG server: an operator over a table's row type depends on the table

An operator whose argument was a table's row type, or that type's array,
was invisible to DROP TABLE. A plain DROP TABLE listed only the function
behind the operator. DROP TABLE ... CASCADE then dropped the table and the
function and left the operator in `pg_operator`, over a type that no longer
existed. PostgreSQL records a dependency from an operator to each of its
types. The table's DROP therefore names the operator and its CASCADE drops
it, and this server now does the same.

#### Fixed

- DROP TABLE, DROP VIEW and DROP MATERIALIZED VIEW refuse with 2BP01 while
  an operator names the relation's row type or its array, and their CASCADE
  drops the operator. Each operator is described as `getObjectDescription`
  describes it (`operator ====(t,t)`, with `NONE` for a prefix operator's
  missing left argument). The operators fall among the routines in
  creation order, in the DETAIL and the CASCADE notice alike.
- CREATE OPERATOR draws its oid from the oid counter, as `OperatorCreate`
  does, and `pg_operator.oid` reports it. An operator created before this
  change keeps the oid its name hashes to.

Fixes RP-561 (tests/backlog/test_rp_found_rp546.py::test_operator_over_row_type_depends_on_its_table,
tests/backlog/test_rp_found_rp546.py::test_operators_over_a_row_type_in_creation_order).
Files RP-893: DROP FUNCTION of an operator's function leaves the operator.
