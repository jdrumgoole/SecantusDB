### Enum casts describe like PostgreSQL's

An unaliased cast to a user-defined type now names its output column after
the type — `SELECT 'hi'::te` yields a column named `te`, matching PG's rule
for enums, composites, and domains — and RowDescription reports
DataTypeSize 4 for enum-typed columns, mirroring how PostgreSQL stores enum
values as 4-byte oids. The pgtest `enum` corpus file pins both shapes and
is now green.

#### Fixed
- `SELECT 'x'::myenum` reported `?column?` with size -1; now the type name
  with typlen 4.
