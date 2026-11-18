### COMMENT ON reaches every built-in catalog object

`COMMENT ON` a built-in conversion, language, collation, text search parser
or template, the `pg_catalog`, `information_schema` and `pg_toast` schemas, or
the `plpgsql` extension now replaces initdb's description, keyed by the
object's oid as PostgreSQL's `pg_description` keys it, and `IS NULL` removes
it. Before, a built-in conversion was "does not exist", the language,
collation and text search kinds were refused, and the schema and extension
comments were accepted but never shown.

#### Fixed

- Rust PG server: `COMMENT ON CONVERSION | LANGUAGE | COLLATION | TEXT SEARCH
  PARSER | TEXT SEARCH TEMPLATE` over a built-in object, and on the built-in
  schemas and `plpgsql`, replace (or with `IS NULL` delete) initdb's
  description.

Fixes RP-969 (tests/backlog/test_rp_found_rp514.py::test_comment_on_other_builtin_catalogs)
