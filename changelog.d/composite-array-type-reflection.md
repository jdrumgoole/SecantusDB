### Composite columns and composite-array type names in the catalog

A composite-typed column (`c custom`) reported `pg_attribute.atttypid`
2249 (generic RECORD) instead of the composite's minted oid, so
getColumns / psycopg composite reflection couldn't resolve its type
name. Composite columns now report their minted oid (and
composite-array columns the array-companion oid). Array type names also
avoid collision the way real PG does: when `_custom` is already a type
(a composite named `_custom`), the array type of `custom` becomes
`__custom` rather than shadowing it. pgjdbc's customArrayTypeInfo.

#### Fixed
- `pg_attribute.atttypid` for composite / composite-array columns is the
  minted type oid, not RECORD/text.
- Array type names in `pg_type` avoid collisions with existing type
  names by prepending underscores in element-oid (creation) order
  (`custom[]` → `__custom`, `_custom[]` → `___custom`).
