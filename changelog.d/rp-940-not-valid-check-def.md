### A CHECK added NOT VALID prints NOT VALID

`pg_get_constraintdef` of a table CHECK that has not been validated now ends
with ` NOT VALID`, as PostgreSQL's `pg_get_constraintdef_worker` prints it
while `convalidated` is false: `CHECK ((x > 0)) NOT VALID`. Tools that
read constraint definitions back, `psql`'s `\d` and `pg_dump` among them,
now see that the constraint is not yet validated.

#### Fixed

- Rust PG server: `pg_get_constraintdef` keeps `NOT VALID` on an
  unvalidated table CHECK.
  Fixes RP-940 (tests/backlog/test_rp_found_rp860.py::test_a_not_valid_check_prints_not_valid)
