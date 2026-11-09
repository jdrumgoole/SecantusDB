### Rust PG server: COPY FROM reads each field at its column's modifier

`COPY ... FROM STDIN` stored every field as written, whatever the column's
modifier: `abcdef` went into a `varchar(3)` column, `1.25` kept its digits in
a `numeric(4,1)` one, and a `bit(3)` column took a one-bit string. An INSERT
of the same values was refused or rounded. PostgreSQL's `CopyFrom` reads each
field with `InputFunctionCall(..., typmod)` (a binary field with
`ReceiveFunctionCall(..., typmod)`), so the column never holds a value its
type refuses.

#### Fixed

- A COPY field, text or binary, is now fitted to its column's modifier by
  the rule an INSERT's assignment uses. An overlong `varchar(n)` / `char(n)`
  value is 22001, and its trailing blanks past the width are dropped. A
  `numeric(p,s)` value rounds, and is 22003 past its precision. A `bit(n)`
  of another length is 22026 and an overlong `varbit(n)` is 22001. A
  `timestamp(p)` / `time(p)` value rounds to its precision. Each element of
  an array column is fitted the same way.
- A binary COPY field's error now names its line and column in CONTEXT
  (`COPY t, line 1, column s`), as PostgreSQL 15.19's does.

Fixes RP-718 (tests/backlog/test_rp_found_rp710.py::test_copy_reads_a_field_at_its_columns_modifier)
