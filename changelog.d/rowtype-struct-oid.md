### ResultSetMetaData fidelity: STRUCT oids, base columns, declared typmods

Three metadata gaps JDBC's ResultSetMetaData surfaces. A column typed by
a table's row type describes with the table's rowtype oid (typtype 'c' —
drivers map it to `Types.STRUCT`; the generic RECORD oid mapped to
OTHER). Bare-column outputs carry their source table oid and attnum even
when the SELECT list mixes in computed expressions, and an aliased
column resolves to its base column — `getBaseColumnName` works. Declared
type identities ride the descriptors: `varchar(n)` reports its real oid
and length typmod (display size), `timestamp(p)` its precision,
`numeric(p,s)` its packed precision/scale.

#### Fixed
- Table-rowtype columns: rowtype oid (typtype 'c'), not RECORD/2249.
- Base-column table_oid/attnum on evaluated (computed-projection)
  selects; aliases resolve to their source column.
- varchar/bpchar report their declared oid + typmod; timestamp(p) and
  numeric(p,s) typmods flow to RowDescription.
