### Rust pgserver: enum columns report their own oid

A SELECT of an enum COLUMN was described over the wire as `varchar` (oid 1043)
instead of the enum's own type oid. psycopg reads that oid to decide whether to
apply a registered enum loader, so with `varchar` it handed back a bare string
where a `register_enum`'d client expected the Python enum member — the enum
value-mismatch and non-ASCII case-fold failures. The column-description paths
now consult the user-type catalog (as the parameter path already did), so an
enum column — scalar or array — carries the enum's minted oid, and the label is
returned verbatim.

#### Fixed
- Enum columns (and enum-array columns) are described with the enum type's oid,
  not `varchar`, so psycopg's registered enum loader applies and returns the
  enum member.
