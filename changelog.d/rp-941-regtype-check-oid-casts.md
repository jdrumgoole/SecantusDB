### A `regtype` comparison in a CHECK prints as PostgreSQL prints it

`regtype` and the other OID alias types have no `=` of their own, so
PostgreSQL compares two of them as `oid` and its deparser prints the casts:
`check (pg_typeof(x) = 'int4'::regtype)` reads back as `CHECK
(((pg_typeof(x))::oid = ('integer'::regtype)::oid))`. The Rust PostgreSQL
server now prints the same, with the `regtype` constant in its canonical
name.

#### Fixed

- `pg_get_constraintdef` (and every other ruleutils print) of a comparison
  between two `reg*` values prints their `::oid` casts, and a `regtype`
  constant prints as `regtypeout` names it.
  Fixes RP-941 (tests/backlog/test_rp_found_rp860.py::test_a_regtype_comparison_in_a_check_prints_its_oid_casts)
