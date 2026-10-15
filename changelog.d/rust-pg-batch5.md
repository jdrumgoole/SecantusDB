### Rust PostgreSQL server: backlog batch 5

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Fixed

- A `char(n)` field too long for a row type is placed at the text literal
  it came from (`'(abcd,x)'::t`, 22001 at 8), and a RECORD cast to the type
  truncates it as an explicit cast does (`row('abcd', 'x')::t` is
  `(abc,x)`), where it had been refused. Fixes RP-032
  (`tests/backlog/test_rust_postgres_gaps.py::test_row_type_char_overflow_has_a_position`).

- A cancel that lands while a large statement is still being PARSED is
  honoured at once, as PostgreSQL honours it (57014 0.2 s after a cancel
  0.5 s into a 1.2 MB statement, where it had waited for the 2.4 s parse).
  A statement over 64 KiB is parsed on a thread of its own while the
  planner polls the backend's cancel flag (`secantus_pgplan::cancel`),
  through every pass that parses it first (the table locks, the privilege
  check); the flag is only looked at there, and a parse it cut short is not
  memoised. A Query message's cycle now opens at its first byte (a
  `MessageStartHook` in the vendored pgwire), so a cancel sent while a long
  message is still arriving is kept rather than cleared as stale, and a
  57014 carries no position, which PostgreSQL never gives it and which
  took a 1.2 MB scan to look for. Fixes RP-002
  (`tests/backlog/test_rp_triage.py::test_cancel_during_parse_is_honoured_at_once`).

- `create type int` (and `integer`, `smallint`, `bigint`, `real`, `double
  precision`, `boolean`, `character varying`, `decimal` and the other
  grammar spellings with no `pg_type` row) creates `public."int"` beside the
  built-in, as PostgreSQL 15.19 does, where it was 42710. A bare `int` still
  means `int4`; `"int"` and `public."int"` reach the new type, which prints
  `"int"`. The type is held under its quoted spelling, as `"char"` is
  (`pgtypes::user_type_key`). Fixes RP-038
  (`tests/backlog/test_rust_postgres_gaps.py::test_create_type_named_int`).

- A 42704 for an unknown type is placed at a qualified name's first part
  (`'a'::public.nosuch` at 13) and at the literal a `regtype` reads
  (`'nosuch'::regtype` at 8), as PostgreSQL places them. Part of RP-041
  (`tests/backlog/test_rust_postgres_gaps.py::test_unknown_type_error_position`).

- `DROP TYPE` over an enum, composite or range checks what depends on its
  values, as PostgreSQL does: columns of the type or of its array type, a
  domain over it and the domain's columns, and fields of composite types.
  Without CASCADE it is 2BP01 naming each in PostgreSQL's order; with it,
  they are dropped under one notice. The type had been dropped from under
  its columns, which kept their values typed as a type that no longer
  existed, and CASCADE left them. Part of RP-041
  (`tests/backlog/test_rust_postgres_gaps.py::test_drop_type_cascade_names_the_column`).
- `DROP DOMAIN ... CASCADE` over two columns of one table dropped only the
  first: the second read back as NULL. It drops every column, in one
  `ALTER` per table, and sends PostgreSQL's `drop cascades to` notice,
  which it had not sent at all. The extension drop that removes a type's
  columns had the same per-column loop, and is fixed with it
  (`::test_drop_domain_cascade_drops_every_column`).

- An enum compared with an untyped literal reads it as the enum when the
  statement is analysed, as PostgreSQL does: `c = 'zz'` is 22P02 at the
  literal even over an empty table (it had answered no rows), in `=`, `<`,
  `IN` and `IS DISTINCT FROM`. An enum beside any other typed operand
  (`c = d::text`) is 42883, where it had compared the label text. Part of
  RP-041 (`tests/backlog/test_rust_postgres_gaps.py::test_enum_comparison_reads_its_literal_at_plan_time`).

- A domain over an enum (or over a domain over one) is described with the
  enum's oid, as PostgreSQL sends it, where it went out as varchar (1043).
  Part of RP-041
  (`tests/backlog/test_rust_postgres_gaps.py::test_domain_over_enum_is_described_as_the_enum`).

- `pg_type.oid` and `pg_type.typarray` are described as `oid` (26), as on
  PostgreSQL, where they went out as int8 (20): the divergence every full
  corpus sweep had reported, in `b38_misc` and `b40_types`, which no
  backlog item recorded. Those two corpora pin it.

- The 59 built-ins PostgreSQL 15.19 ships with EXECUTE revoked from
  PUBLIC -- `pg_reload_conf`, `pg_stat_reset`, the `pg_ls_*dir` and
  `pg_read_file` family, `lo_import` / `lo_export` and the rest -- are
  refused (42501) to a role without a GRANT or the predefined role
  (`pg_monitor`, `pg_read_all_stats`) PostgreSQL grants them to, for the
  role in effect at the call. Any role could call them, and
  `pg_reload_conf()` judged by the login role and ignored a GRANT. Part of
  RP-013 (`tests/backlog/test_rust_postgres_gaps.py::test_restricted_builtin_needs_a_grant`).

- `pg_stat_ssl` and `ssl_client_dn` name a certificate's attributes by
  OpenSSL's own short names, as PostgreSQL (linked against OpenSSL) prints
  them: all 1,341 objects OpenSSL 3.6.3 knows (`businessCategory`,
  `organizationIdentifier`, `jurisdictionL`, ...), dumped from
  `OBJ_nid2sn` into `x509_short_names.tsv`, where 18 were listed and the
  rest printed `UNDEF`. Part of RP-013 (the `short_names_are_openssls`
  unit test in `crates/secantus-pg/src/x509.rs`).

- `ts_rewrite(query, select)` checks that its SELECT describes two tsquery
  columns before it runs it, as PostgreSQL's SPI does: over no rows (`...
  where false`) a wrong shape is 22023, where it had answered the query
  unchanged. It asks the server for the SELECT's described columns
  (`correlated::describe_sql`). Part of RP-015
  (`tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed[ts-rewrite-no-rows]`).
- The XML schema exports -- `query_to_xmlschema`, `table_to_xmlschema`,
  the `*_and_xmlschema` forms, `cursor_to_xml` / `cursor_to_xmlschema` and
  the schema and database forms -- transcribed from PostgreSQL 15's
  xml.c (`map_sql_table_to_xmlschema`, `map_sql_type_to_xmlschema_type`),
  over the query's described column types. `query_to_xml` also puts an
  `xml` column's value in raw, where it had escaped it, and `xmlbinary =
  hex` is honoured. Part of RP-015
  (`::test_builtin_coverage_fixed[query-to-xmlschema]` and the cases
  beside it; corpus `xml_schema_export`).

- A schema created inside a transaction block can hold a table in the same
  block: `create schema s; create table s.t (...)` had been 3F000, the
  schema not noted for the rest of the transaction as every other CREATE
  is.
- `has_schema_privilege` takes a schema's oid (and a `regnamespace`), and
  is NULL for an oid naming none, where the oid was read as a name.
- `DECLARE ... CURSOR WITH HOLD` is taken outside a transaction block, as
  PostgreSQL takes it (only a cursor without HOLD needs one), and `CLOSE
  ALL` is tagged `CLOSE CURSOR ALL`.
- A column of an enum in another schema (`s.e`) is listed in
  `pg_attribute`, described with the enum's oid where it went out as
  varchar, and named by its schema in `information_schema.columns`
  (`udt_schema`, `udt_name`) for a table and a view.
  These four were found beside RP-015's XML work, filed as RP-042 and fixed
  in this change (`tests/backlog/test_rust_postgres_gaps.py::test_schema_created_in_a_block_holds_a_table`,
  `::test_has_schema_privilege_by_oid`, `::test_declare_with_hold_outside_a_block`,
  `::test_enum_outside_public_is_a_column_type`).

#### Added

- The language validators `fmgr_internal_validator`, `fmgr_c_validator`,
  `fmgr_sql_validator` and `plpgsql_validator`, and
  `postgresql_fdw_validator`, transcribed from PostgreSQL 15
  (`CheckFunctionValidatorAccess`, foreign.c): `void` for a function of the
  validator's language, 42501 naming the language's own validator
  otherwise, 42883 for an oid naming no function, and the libpq options a
  foreign server or user mapping may set. Part of RP-015
  (`tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed[fmgr-validator]`,
  `[fdw-validator]`).
- Corpora `row_type_char_overflow`, `fmgr_validators`, `type_named_like_grammar`, `drop_type_dependents`, `enum_literal_analysis`, `restricted_builtins` and `xml_schema_export`.
