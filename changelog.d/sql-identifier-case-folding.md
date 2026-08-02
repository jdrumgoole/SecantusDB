### Unquoted identifiers fold to lower case, as Postgres does

`SELECT r.table_name FROM (SELECT id AS TABLE_NAME …) r` reported that the
column did not exist. Postgres lower-cases an unquoted identifier, so writing
an alias in upper case and reading it back in lower case names the same column;
we compared every spelling exactly, so the two forms were two different names.

Quoted identifiers keep their spelling exactly, which is what quoting is for —
`"Mixed"` and `mixed` remain different columns.

Matching case always worked, which is why this went unnoticed: code that writes
an alias one way and reads it back the same way never trips it. Generated SQL,
and anything written in the SQL-standard upper case, does — JDBC's metadata
queries are how it surfaced.

Folding happens once, immediately after parsing, so table names, column
references and aliases all agree on one canonical spelling.

Note for existing databases: a table created unquoted with a mixed-case name is
now addressed lower-cased, matching what Postgres would have stored in the
first place.

#### Fixed

- An unquoted identifier written in one case and read in another now names the
  same table, column or alias.
