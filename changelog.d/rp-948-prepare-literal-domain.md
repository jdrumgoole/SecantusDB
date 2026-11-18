### PREPARE raises a composite or array literal's field domain error

PostgreSQL reads every string literal a cast types while it analyses a
statement, and the input functions of a composite and an array check each
field's or element's domain as they read it. So `prepare p as select
'(0,x)'::ctp`, where `ctp`'s first field is a domain requiring a positive
value, fails at PREPARE with 23514 at the literal, whether or not a row would
ever reach it. The Rust PostgreSQL server's analysis-time literal check
counted only input errors, so such a PREPARE succeeded and the error waited
for EXECUTE. It now raises the field's domain error at the literal; a cast
straight to a domain still checks it only when the expression runs, as there.

#### Fixed

- `PREPARE` of a statement with a composite or array literal whose field or
  element violates its domain is 23514 (or 23502) at the literal.

Fixes RP-948 (tests/backlog/test_rp_found_rp552.py::test_prepare_error_location)
