### ALTER COLUMN TYPE to a domain over the column's type

`ALTER TABLE ... ALTER COLUMN a TYPE pint`, where `a` is an `int` column and
`pint` a domain over `int`, now retypes the column without a `USING` clause,
as PostgreSQL does: the assignment-cast check sees a domain on either side
through to its base type before looking for a cast, and the rows already in
the table are checked against the domain's constraints on the way in.

#### Fixed

- `ALTER COLUMN TYPE` to a domain over the column's type was refused with
  42804 `cannot be cast automatically`.
  Fixes RP-556 (tests/backlog/test_rp_found_rp529.py::test_alter_column_type_to_domain)
