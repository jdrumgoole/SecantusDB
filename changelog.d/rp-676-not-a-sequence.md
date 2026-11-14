### A sequence function over a relation that is not a sequence says so

`nextval`, `currval` and `setval` now open the relation they are given the
way PostgreSQL's `init_sequence` does: a table or catalog relation named by
text or by `regclass` is 42809 `"pg_class" is not a sequence`, with no
position, and an oid no relation has is XX000 `could not open relation with
OID n`. Each was 42P01 `relation ... does not exist` before. A sequence
named by its oid (`nextval('s'::regclass::oid)`) now advances, where it was
42P01 too.

#### Fixed

- Rust PG server: `nextval` / `currval` / `setval` over a non-sequence
  relation, or a bare oid, answer PostgreSQL 15.19's 42809 / XX000.
  Fixes RP-676 (tests/backlog/test_rp_found_rp458.py::test_sequence_function_over_a_relation_not_a_sequence)
