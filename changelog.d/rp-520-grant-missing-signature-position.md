### Rust PG server: GRANT and REVOKE report a missing routine with no position

A GRANT or REVOKE naming a routine signature no routine has, such as
`grant execute on function gc(bool) to public`, is 42883 `function
gc(boolean) does not exist` with no position, as PostgreSQL 15.19's
`LookupFuncWithArgs` reports it. It carried one, at the routine's name.

#### Fixed

- GRANT / REVOKE on a FUNCTION, PROCEDURE or ROUTINE whose signature does
  not exist no longer positions the 42883 at the name, as DROP, ALTER and
  COMMENT already did not.

Fixes RP-520 (tests/backlog/test_rp_found_rp514.py::test_grant_missing_signature_has_no_position)
