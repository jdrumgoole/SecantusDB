### Rust pgserver: the bytea type

The Rust PostgreSQL server now supports `bytea` — as a cast, a bound parameter
(text or binary wire format), and a real column type (oid 17). A value is
stored as a `Binary` (the representation the Python server uses, since the two
share one store), accepts both PostgreSQL input forms (the `\x…` hex form and
the octal-`\ooo` escape form), and renders back as the `\x…` hex text a modern
server emits. Because psycopg sends and reads a `bytea` in the binary format by
default, both the binary parameter decoder and the binary result encoder handle
it.

The byte-level functions come with it: `length` / `octet_length` / `bit_length`
count bytes, `get_byte` / `set_byte` read and replace a byte, `encode` /
`decode` convert to and from `hex` / `base64` / `escape`, and `bytea || bytea`
concatenates. The error surface matches PostgreSQL: a bad hex digit or odd
length is `22023`, a malformed escape is `22P02`, and a byte index out of range
is `2202E`.

#### Added
- `bytea` cast, column type (oid 17), and text/binary parameter and result
  wire formats.
- `get_byte`, `set_byte`, `encode`, `decode`, `bytea || bytea`, and
  bytea-aware `length` / `octet_length` / `bit_length`.
