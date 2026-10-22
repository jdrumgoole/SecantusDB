### Rust PostgreSQL server: column statistics, role settings, and who owns what

ANALYZE's column statistics are served as PostgreSQL stores them. `ALTER
ROLE ... SET`, `ALTER TABLE`'s storage options, `REASSIGN OWNED` and `DROP
OWNED` run as PostgreSQL 15.19 runs them. DROP ROLE now refuses a role that
something still depends on, where it used to drop it and leave the object
behind with an owner that no longer exists.

#### Added

- `pg_statistic`: each column ANALYZE measured, its scalars and its slots in
  `compute_scalar_stats`' order -- the most common values under the type's
  `=`, the histogram's bounds and the correlation under `<`, with the
  collation of a text type -- and `pg_stats`, PostgreSQL's own view over it.
  For the types ANALYZE samples here: integers, booleans, floats, `text` /
  `varchar` and `char(n)`.
- An `anyarray` column is sent as `array_out` writes it (`{"a  "}`), and
  `NULL::anyarray` (and the other polymorphic pseudo-types) is a value.
- `ALTER TABLE ... SET LOGGED / UNLOGGED`, `SET (...)` / `RESET (...)`
  with PostgreSQL's heap options and their range checks, `ALTER COLUMN ...
  SET STORAGE` (0A000 for a fixed-length type) and `SET STATISTICS` (capped
  at 10000 with PostgreSQL's warning). `pg_class.relpersistence` /
  `reloptions` and `pg_attribute.attstorage` / `attstattarget` show them.
- `ALTER ROLE r [IN DATABASE d] SET / RESET`, and `ALTER ROLE ALL`: kept
  in `pg_db_role_setting`, and applied at login in `process_settings`'
  order -- every role's, the database's, the role's, the role's in this
  database. `pg_settings.source` names the layer (`global`, `database`,
  `user`, `database user`), and `session` after a SET. DROP ROLE and DROP
  DATABASE forget the defaults they held.
- `REASSIGN OWNED BY ... TO ...` and `DROP OWNED BY ... [CASCADE]`, over the
  `pg_shdepend` rows this server serves. They check privileges and refuse
  the bootstrap superuser's objects as PostgreSQL does. DROP OWNED revokes
  the role's privileges, then drops its objects as one deletion: an owned
  object that another owned object depends on is no obstacle.
- `ALTER SEQUENCE ... OWNER TO` and `ALTER SCHEMA ... OWNER TO`.

#### Fixed

- RP-099 is down to extended statistics
  (`tests/backlog/test_rp_batch26.py::test_scenario[ANALYZE computes statistics]`
  now pins `pg_stats`).
- RP-072 is down to `ALTER SYSTEM`, `ALTER TYPE ... ADD / RENAME
  ATTRIBUTE`, `CREATE TEXT SEARCH CONFIGURATION` and `WHERE CURRENT OF`
  (`tests/backlog/test_rp_batch30.py::test_scenario[ALTER ROLE SET]`,
  `[ALTER TABLE storage options]`, `[REASSIGN OWNED and DROP OWNED]`,
  `[REASSIGN OWNED needs both roles' privileges]` and
  `::test_role_setting_sources` pin what was built).
- Fixes RP-133 (`tests/backlog/test_rp_batch30.py::test_scenario[pg_settings in the parameter's unit]`):
  `pg_settings.setting` is in the parameter's own unit (`work_mem = '8MB'`
  reads `8192`), not as SHOW writes it.
- Fixes RP-134 (`tests/backlog/test_rp_batch30.py::test_scenario[DROP ROLE of a role something depends on]`):
  DROP ROLE of a role that owns an object, or that an ACL names, is 2BP01
  with PostgreSQL's DETAIL (`owner of table t`, `privileges for schema s`,
  `N objects in database d`). It used to drop the role.
- Fixes RP-135 (`tests/backlog/test_rp_batch30.py::test_scenario[an index, a row type and an array type follow their owner]`):
  an index and a composite type's relation are their table's or type's
  owner's, a row type its relation's, and an array type its element's.
  ALTER TABLE ... OWNER TO also moves the sequences the table's columns
  own. `pg_shdepend` no longer lists the objects PostgreSQL never records
  an owner for, and now lists schemas.
- Fixes RP-136 (`tests/backlog/test_rp_batch30.py::test_scenario[ALTER SEQUENCE and ALTER SCHEMA OWNER TO]`):
  both forms were refused. A sequence a column owns changes owner only with
  its table (0A000, `Sequence "s" is linked to table "t".`).
- Fixes RP-138 (`tests/backlog/test_rp_batch30.py::test_scenario[GRANT on a routine of the other kind]`):
  GRANT / REVOKE `ON FUNCTION` naming a procedure, or `ON PROCEDURE` naming
  a function, is 42809.
- Fixes RP-139 (`tests/backlog/test_rp_batch30.py::test_scenario[a sequence made in the open block is a relation]`):
  a sequence made in the open block can be read and altered as a relation
  before COMMIT (`select * from t_a_seq` was 42P01).
- Fixes RP-140 (`tests/backlog/test_rp_batch30.py::test_scenario[a sequence's relacl]`):
  a sequence's grants show in `pg_class.relacl`.
- Fixes RP-141 (`tests/backlog/test_rp_batch30.py::test_scenario[a DO block drops a view, then its table]`):
  a statement inside a PL/pgSQL body sees the catalog the statement before
  it changed. `drop view v; drop table t` in one DO block was refused
  because the view still depended on the table. An error raised by a
  statement the server runs for itself keeps its HINT.
- Fixes RP-142 (`tests/backlog/test_rp_batch30.py::test_scenario[a routine's proacl]`):
  `pg_proc.proacl` shows a routine's grants in ACL order (it was always
  NULL).

#### Known

- RP-137, filed: a sequence's, view's and index's oid is a hash of its name,
  so oids do not rise in creation order as PostgreSQL's do, and DROP ROLE's
  DETAIL (sorted by oid, as PostgreSQL sorts it) lists the objects in
  another order
  (`tests/backlog/test_rp_batch30.py::test_scenario[DROP ROLE lists dependents in creation order]`).
