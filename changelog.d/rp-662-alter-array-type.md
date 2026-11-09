### Rust PG server: ALTER COLUMN TYPE without USING follows PostgreSQL's assignment casts

`ALTER TABLE ... ALTER COLUMN ... TYPE` with no `USING` clause now decides
whether the change is allowed the same way PostgreSQL 15 does. Before, a
hand-written table of type families decided it, and it got 139 of 2025
measured type pairs wrong.

#### Fixed

- An array column now changes to another array type when the element types
  have an assignment cast, as in PostgreSQL's `array_coerce` path. Examples:
  `int[]` to `int8[]`, `smallint[]` to `int[]` and `int8[]` to
  `numeric[]`. The values are kept. Each of these was refused with 42804
  `cannot be cast automatically`.
- The decision now follows PostgreSQL's `find_coercion_pathway` in the
  assignment context:
  - a `pg_cast` row (built in, or a user's `CREATE CAST`) decides by its
    context alone;
  - with no row, two arrays coerce through their elements;
  - otherwise only an assignment to a string type goes through I/O.
- Pairs the family table answered wrongly now match PostgreSQL 15.19:
  - now allowed: `int4` to `oid` and `int4` to `money`;
  - now refused with 42804: `int4` to `"char"` and `date` to `time`.
- A domain on either side is still refused; that remains RP-556.

Fixes RP-662 (tests/backlog/test_rp_found_rp658.py::test_alter_type_between_array_types)
