### Rust PostgreSQL server: PostgreSQL's operator classes, pseudo-types and statistics types

`pg_opclass` now lists PostgreSQL 15.19's 177 operator classes, and an
index's explicit class is resolved against them. The `regtype` type names
every pseudo-type. The statistics types exist as PostgreSQL has them.

#### Fixed

- Fixes RP-183 (`tests/backlog/test_rp_batch40.py::test_scenario[an operator class for another type]`):
  a class the method has, named for a key of another type, was said not to
  exist. It is now 42804 `operator class "jsonb_ops" does not accept data
  type integer[]`.
- Fixes RP-187 (`tests/backlog/test_rp_batch41.py::test_scenario[an explicit operator class for the key's type]`):
  an explicit btree or hash class was refused unless it was one of the
  pattern classes. That covered `int4_ops`, `text_ops` on `varchar`,
  `array_ops` and the rest. A class is now taken as `ResolveOpClass` takes
  it:
  - **Accepted:** its input type, `varchar` for `text`'s, or a polymorphic
    class the key is an instance of.
  - **Printed:** in `indexdef` only when it is not the type's default.
- Fixes RP-188 (`tests/backlog/test_rp_batch41.py::test_scenario[pg_opclass lists every built-in operator class]`):
  `pg_opclass` held only btree's default classes. It is now the 15.19 dump
  (`pg15_catalog_rows.txt`, from `tools/probes/pg15_system_catalog_dump.py`).
- Fixes RP-189 (`tests/backlog/test_rp_batch41.py::test_scenario[regtype names the pseudo-types]`):
  `2277::regtype` printed `2277`, and `'anyarray'::regtype` was 42704. All
  26 pseudo-types (`anyarray`, `anyelement`, `internal`, `"any"`,
  `record[]`, `table_am_handler`, ...) now read and print by name.
- Fixes RP-186 (`tests/backlog/test_rp_batch40.py::test_scenario[the statistics types are types]`):
  `pg_ndistinct`, `pg_dependencies`, `pg_mcv_list`, `pg_brin_bloom_summary`
  and `pg_brin_minmax_multi_summary` were not types. A column may now be
  one. Every value for one is 0A000 `cannot accept a value of type ...`, at
  the literal, and an array of one is 42704, as on PostgreSQL.
