### Result columns report the table and column they came from

Every result column described itself as having no source: the table OID and
column number that Postgres puts in each field of a row description were sent
as zero. JDBC clients use exactly those to map a result column back to the
column it was selected from, so an updatable `ResultSet` could not name the
column it was asked to update — it built `UPDATE t SET "" = ?` and the server
rejected it.

Columns selected from a table now carry their source table and position, and
they keep it through aliasing and reordering, since the position describes the
table rather than the select list. Computed columns still report none, which is
what Postgres reports for them.

#### Fixed

- Updating a row through a JDBC updatable `ResultSet` no longer fails with
  `column "" does not exist`.
