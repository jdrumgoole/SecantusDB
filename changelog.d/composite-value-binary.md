### Composite values in the binary wire format, and arrays of composites

The Rust PostgreSQL server (`secantusd-pg`) now hands a composite value back the
way PostgreSQL does when a client asks for it in binary. A `SELECT
row('hi', 10, 20)::mytype` on a binary cursor used to come back as the text
`(hi,10,20)` — every field a string — because the composite had no binary
encoder. It now goes out in PostgreSQL's binary record format (a field count,
then each field's oid, length and bytes), so psycopg's binary composite loader
decodes each field to its real Python type: the `float8` is a `float`, the
`int8` an `int`. Nested composites recurse, so a composite whose field is itself
a composite round-trips in binary too.

An array of composites — `SELECT array[row('hi', 10, 30)::mytype]` — now reports
the composite's array oid rather than falling back to varchar. Before, the whole
array arrived as the 17-character string `{"(hi,10,30)"}`; now a registered
client parses it into a one-element list of composites, in both the text and the
binary wire formats.

#### Added

- `secantus-pgserver`: composite / composite-array binary result encoding
  (`encode_binary` / `element_binary` / a new `record_binary` helper) and a
  composite ARRAY arm in `user_wire_type` reporting the derived `typarray` oid.
  Composite wire types now carry `Kind::Composite` with their declared field
  types, so the binary encoder knows each field's oid.

#### Fixed

- `secantus-pgserver`: a composite-array result in a TEXT cursor was
  double-escaped (its element oid is a user oid the generic array path does not
  know, so it hit the catch-all) — it now renders each element's `(...)` text
  and escapes once, matching PostgreSQL.
