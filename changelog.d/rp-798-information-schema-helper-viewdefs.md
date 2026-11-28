### `pg_get_viewdef` prints the `information_schema` views calling its helpers

`pg_get_viewdef` now deparses `information_schema.domains`, `sequences`,
`attributes`, `columns` and `user_mapping_options` exactly as PostgreSQL
15 prints them, in both print modes; they were NULL. That makes 60 of the
65 `information_schema` views. The printer now handles what those views
needed:

- a call of one of the schema's SQL helpers (`_pg_char_max_length`,
  `_pg_numeric_precision`, `_pg_truetypid`, ...) prints qualified by its
  schema, as `generate_function_name` qualifies a function outside the
  search path, and types as its `pg_proc` row says;
- a whole-row argument, written `a` or `a.*`, prints as `a.*`, and only
  over the relation the parameter's row type names
  (`information_schema._pg_truetypid(a.*, t.*)`);
- a built-in called over an `information_schema` domain resolves on the
  domain's base type and shows the relabel PostgreSQL adds
  (`pg_has_role((um.srvowner)::name, 'USAGE'::text)`), as operators
  already did.

The five views over the set-returning `_pg_expandarray`, and six
`pg_catalog` views, are still NULL rather than printed wrong.

#### Fixed

- `pg_get_viewdef` of five `information_schema` views was NULL.
  Fixes RP-798, in part (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_views_calling_information_schema_helpers)
