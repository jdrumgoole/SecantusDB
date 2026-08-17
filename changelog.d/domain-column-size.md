### getColumns reports the size of domain columns

A domain over a length/precision-qualified base type (`CREATE DOMAIN d
AS varbit(3)` / `numeric(8,3)`) now carries that typmod on its
`pg_type` row (`typtypmod` + `typbasetype`), which is where JDBC's
getColumns reads a domain column's COLUMN_SIZE. Previously the domain's
typtypmod was always -1, so a `varbit(3)` domain column reported an
unbounded size instead of 3 (pgjdbc's domainColumnSize).

#### Fixed
- Domain `pg_type.typtypmod` / `typbasetype` reflect the base type's
  declared length/precision, so getColumns reports COLUMN_SIZE /
  DECIMAL_DIGITS for domain columns.
