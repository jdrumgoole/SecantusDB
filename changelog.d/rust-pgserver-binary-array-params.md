### Rust pgserver: binary array parameters for bytea/inet/cidr/uuid

`bytea[]`, `inet[]`, `cidr[]`, and `uuid[]` now round-trip as BINARY array
parameters (oids 1001 / 651 / 1041 / 2951) and as array columns read in either
wire format. The per-element decode reuses the scalar bytea / inet / cidr /
uuid decoders, and the array types report their own oid instead of falling
through to `varchar` (which had made a binary result hit the binary-varchar
encoder and a text result hand back strings). A `bytea` array element also
renders as `\x…` hex in the array text form.

#### Added
- Binary array-parameter decode and correct result typing for `bytea[]`,
  `inet[]`, `cidr[]`, `uuid[]`.

#### Fixed
- A `bytea[]` cast to text rendered each element as Rust debug output; it now
  renders `\x…` hex like PostgreSQL.
