### Table-rowtype columns describe as their composite type

A column typed by a table's row type now describes on the wire with the
table's rowtype oid — whose `pg_type` row carries `typtype 'c'` — instead
of the generic RECORD oid. JDBC metadata maps such a column to
`java.sql.Types.STRUCT`, as PostgreSQL does (pgjdbc's
ResultSetMetaDataTest composite trio).

#### Fixed
- RowDescription reports the rowtype oid for table-typed columns
  (was RECORD/2249, which drivers map to OTHER).
