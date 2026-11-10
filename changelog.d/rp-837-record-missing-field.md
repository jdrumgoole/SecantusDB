### Rust PG server: reading a field a PL/pgSQL record lacks is 42703

A PL/pgSQL expression that read a field its record variable does not have
(`r.nosuch`, where `r` was filled by `select 1 as a into r` or declared of
a table's row type) left the reference in the SQL text, which then read
`r` as a FROM item: `42P01 missing FROM-clause entry for table "r"`.
PostgreSQL builds a field datum for any `r.f` and looks the field up in the
record's row type when it is read, so the answer is `42703 record "r" has
no field "nosuch"`, in every statement that evaluates an expression, with
the same CONTEXT lines. A record now also hides a block labelled with its
own name, as `plpgsql_ns_lookup` searches each level for the name before
the level's label.

#### Fixed

- Reading a missing field of a PL/pgSQL record is 42703, not 42P01.

Fixes RP-837 (tests/backlog/test_rp_found_rp689.py::test_reading_a_field_a_record_lacks_is_42703)
