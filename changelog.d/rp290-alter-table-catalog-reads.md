### Rust PG server: ALTER TABLE ADD / DROP COLUMN counts its own catalog reads

`pg_stat_sys_tables` and `pg_stat_sys_indexes` now count the catalog scans
PostgreSQL 15.19 makes for a superuser's `ALTER TABLE ... ADD COLUMN` and
`DROP COLUMN`, where they counted none. The walk follows `ProcessUtilitySlow`
and `tablecmds.c`: the event trigger cache a backend's first DDL builds, the
table's lookup and `RangeVarCallbackForAlterRelation`'s pg_class row,
`ATParseTransformCmd`'s schema name, the column-name check, pg_class's and
pg_attribute's index lists and `pg_class_tblspc_relfilenode_index`'s
descriptor, pg_depend opened even for a pinned type, the table's descriptor
rebuilt at each `CommandCounterIncrement`, `has_subclass`; and for a drop,
`findDependentObjects`' two pg_depend scans, `RemoveStatistics`, and the
pg_depend, pg_shdepend, pg_description, pg_seclabel and pg_init_privs scans
of `deleteOneObject`, with the dropped column's old pg_attribute entries
counted as the dead entries every later scan in the statement meets.

#### Fixed

- An ADD COLUMN or DROP COLUMN counted no catalog reads; another session's
  ALTER is now compared segment by segment instead of left out.
  Fixes RP-290, part (tests/backlog/test_rp_catalog_reads.py::test_gap[an ALTER TABLE's own catalog reads], tests/backlog/test_rp_catalog_reads.py::test_an_alter_table_twice_each_way).
