### Rust pgserver: uuid, and timetz columns

The Rust PostgreSQL server now supports `uuid` — as a cast, a bound value, and
a real column type. Any spelling PostgreSQL accepts (uppercase, surrounding
braces, no hyphens, or hyphens at the standard group boundaries) canonicalises
to lowercase `8-4-4-4-12`, and the column reports its true oid (2950) so a
client hands back a `UUID` object rather than a string. A malformed uuid is
`22P02`, matching PostgreSQL.

`timetz` also works as a column now. Unlike `timestamptz` — whose text is
session-relative and which stays refused until its stored form is an instant —
a `timetz` offset is literal (`12:34:56+02` renders the same under any zone),
so its canonical text is a safe column.

#### Added
- `uuid` cast, bound value, and column type (oid 2950), with PostgreSQL's
  input leniency and `22P02` on malformed input.
- `timetz` as a column type (oid 1266), stored as canonical text.
