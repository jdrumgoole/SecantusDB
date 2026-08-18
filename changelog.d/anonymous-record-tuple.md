### Anonymous record constructors and binary composite parameters

The `(a, b, …)` parenthesized tuple is now an anonymous record constructor,
like `ROW(a, b, …)` — `SELECT (1::int2, 2::int4, 3::int8, null)` builds a
RECORD value (OID 2249) that encodes and renders correctly in both the binary
and text wire formats, preserving each field's declared type OID (so the binary
record embeds `int2`/`int4`/`int8`, not a collapsed `int8`). A `COLLATE`
applied to a value expression is now a no-op on the value (it only affects
comparison/sort order).

Binary composite bind parameters (`$1::my_type` sent in the binary format) are
decoded through the declared type's field layout and validated with
PostgreSQL's exact wire errors — a truncated header or element is `08P01`, a
wrong column count or element-type mismatch is `42804`, and a declared element
length that overruns the message is `22P03`. A parameter declared as the
generic anonymous `RECORD` type (OID 2249) is rejected with `0A000` (input of
anonymous composite types is not implemented), matching PG. This closes the
pgtest `tuple` corpus file.

#### Added

- `scalar.py`: the `(a, b, …)` tuple record constructor and `COLLATE`
  value-expression handling; record field OIDs derive from the argument AST
  (a bare `NULL` field is the unknown type, OID 705).
- `pgextended.py`: a validating binary-composite parameter decoder
  (`_decode_binary_composite`) raising PG's `08P01` / `42804` / `22P03`; a
  RECORD (2249) parameter is rejected with `0A000`.

#### Fixed

- `planner.py`: a `$1::user_type` cast infers the type's minted OID, so a binary
  composite parameter decodes through the record layout instead of being
  mis-read as text.
- `pgextended.py`: a NULL record field carries its declared type OID in the
  binary encoding (was always text).
