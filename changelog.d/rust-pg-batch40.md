### Rust PostgreSQL server: conversions, access methods, and six more statements

Statements PostgreSQL 15.19 runs, and `pg_dump` emits, now run here:
`CREATE / ALTER / DROP CONVERSION`, `CREATE / DROP ACCESS METHOD`, `ALTER
EXTENSION ... UPDATE`, `ALTER OPERATOR ... OWNER TO` and `ALTER COLLATION
... RENAME TO / REFRESH VERSION`. A btree or hash index over a type with
no operator class for it is refused, as PostgreSQL refuses it.

#### Added

- Part of RP-169
  (`tests/backlog/test_rp_batch40.py::test_scenario[ALTER EXTENSION UPDATE]`,
  `[ALTER OPERATOR OWNER TO]`, `[ALTER COLLATION RENAME and REFRESH VERSION]`,
  `[CREATE CONVERSION and its errors]` and
  `[CREATE ACCESS METHOD and what uses one]`):
  - **`CREATE [DEFAULT] CONVERSION`** stores a row in `pg_conversion`.
    `ALTER CONVERSION` takes `RENAME TO`, `OWNER TO` and `SET SCHEMA`, and
    `COMMENT ON CONVERSION` and `DROP CONVERSION [IF EXISTS]` work too. A
    conversion's function is checked as PostgreSQL checks it: by its
    signature, and by the encoding pair it serves.
  - **`CREATE ACCESS METHOD ... TYPE TABLE | INDEX`** stores a row in
    `pg_am`, its handler checked by return type.
    - A table or materialized view can be made `USING` a table method, and
      `pg_class.relam` names it.
    - `DROP ACCESS METHOD` refuses while such a relation exists (2BP01),
      and with CASCADE drops them.
    - An index method has no operator classes, so every index on one is
      refused, as on PostgreSQL.
  - **`ALTER EXTENSION ... UPDATE [TO v]`** answers that the version is
    already installed, or 22023 for a version with no update path.
  - **`ALTER OPERATOR ... OWNER TO`** changes `oprowner`.
  - **`ALTER COLLATION ... RENAME TO`** keeps the collation's oid and its
    columns. `REFRESH VERSION` answers that the version has not changed.

#### Fixed

- Fixes RP-184 (`tests/backlog/test_rp_batch40.py::test_scenario[a table USING a missing or index access method]`):
  `CREATE TABLE ... USING nosuch` (or `USING btree`) succeeded. It is now
  42704 for a missing method, and 55000 `is not of type TABLE` for an index
  method.
- Fixes RP-185 (`tests/backlog/test_rp_batch40.py::test_scenario[btree and hash refuse a type with no class for them]`):
  a btree or hash index was built over any type. These types have no class
  for those methods (`box`, `json`, `xml`, `point`, ..., and for hash
  `money`, `bit`, `tsvector`, ...). An index over one is now 42704 `data
  type ... has no default operator class`, with PostgreSQL's hint, which
  that error had lacked.

#### Known

- RP-183, filed: an operator class of another type is said not to exist,
  where PostgreSQL says it does not accept the type
  (`tests/backlog/test_rp_batch40.py::test_scenario[an operator class for another type]`).
- RP-186, filed: the statistics types (`pg_ndistinct` and four more) are
  42704 as column types
  (`tests/backlog/test_rp_batch40.py::test_scenario[the statistics types are types]`).
