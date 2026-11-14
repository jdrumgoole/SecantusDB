### A function's and a cast's types print as PostgreSQL formats them

A `regprocedure` now prints as PostgreSQL's `format_procedure` does: the
function's name quoted where it needs it and a type in another schema as
`schema.name`, each part quoted on its own -- `"Q f567"(s567.t)` rather than
`Q f567("s567.t")`. The same type text names a cast over such a type in a
DROP's DETAIL and CASCADE notice (`cast from s9c.t to integer`).

#### Fixed

- `oid::regprocedure::text` quotes the routine name and formats each argument type as `format_type_be`.
- A cast over a type in another schema is described as `cast from s9c.t to integer`.

Fixes RP-567 (tests/backlog/test_rp_found_rp546.py::test_regprocedure_quotes_as_format_procedure)
Fixes RP-951 (tests/backlog/test_rp_found_rp546.py::test_cast_over_a_schema_type_described_as_format_type)
