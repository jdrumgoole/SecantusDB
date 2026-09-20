### The Rust PostgreSQL server keeps a declared width

A `char(4)` column was described to the client as an unsized `bpchar`, and its
value came back as `ab` where PostgreSQL sends `ab  `. Underneath was a
sharper problem: the shared catalog records a declared string type as
`type: "text"` with `decl_oid: 1042` and an `atttypmod`, and the Rust side
modelled neither field. So a `char(n)` or `varchar(n)` column created by the
Python server read back as plain `text` with no width — and one created by the
Rust server was written in a form the Python server read as text in turn. The
values always survived; the declared type did not, in either direction.

Both servers now agree on what a declared width is and where it lives. A
`char(n)` value is blank-padded on the way out (it is stored unpadded, so
`length()` and a cast to `text` keep PostgreSQL's meaning for free), and a
comparison against one ignores trailing blanks on both sides — `varchar` stays
blank-sensitive, as PostgreSQL has it.

The remaining `char(n)` rules — the functions that see the padded form,
pattern matching, and `::char(n)` truncation — are mapped in
`tasks/backlog.md`.

#### Fixed

- `crates/secantus-pgcatalog`: `Column` carries `typmod`, reads and writes
  `decl_oid` the way the Python server does.
- `crates/secantus-pgplan`: `CREATE TABLE` records the declared modifier; a
  literal compared against a `char(n)` column is stripped.
- `crates/secantus-pgserver`: the row description carries the width, and
  `char(n)` output is blank-padded.

#### Testing

- `tests/test_rust_pgserver_slice.py`: padding and the described width,
  blank-insensitive comparison (with `varchar` unaffected), and a table
  created by the Python server read back by the Rust one with its declared
  type intact.
