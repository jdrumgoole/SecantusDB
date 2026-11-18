### Built-in types, operators and access methods carry initdb's descriptions

`obj_description('int4'::regtype, 'pg_type')` now answers `-2 billion to 2
billion integer, 4-byte storage`, as on PostgreSQL 15.19. The Rust PostgreSQL
server serves the `pg_description` rows initdb writes from every catalog's
`.dat` -- types, operators, access methods, collations, conversions,
languages, schemas, extensions and the text-search catalogs -- not only the
built-in routines', each under its own catalog's `classoid`.
`obj_description` also accepts a `regtype` argument.

#### Fixed

- `pg_description` serves initdb's 1,907 descriptions of non-routine
  built-in objects, dumped from PostgreSQL 15.19 into
  `pg15_other_descriptions.tsv` by `tools/probes/pg15_system_catalog_dump.py`.
- `obj_description` takes its oid from a `regtype` value.

Fixes part of RP-969 (tests/backlog/test_rp_found_rp514.py::test_builtin_object_has_initdb_description); COMMENT ON such an object stays open under RP-969.
