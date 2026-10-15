### Rust PostgreSQL server: multi-column UPDATE, RETURNING stars, records with char(n), ICU collations

A second batch of the Rust PostgreSQL server's backlog, each fix pinned by
the backlog test that measured it against PostgreSQL 15.19. Two of the
bugs it found on the way were silent wrong answers: inside a correlated
subquery an outer column took the type of the outer row's first column,
and `UPDATE ... FROM ... RETURNING *` left out the FROM table's columns.

#### Fixed

- `UPDATE ... SET (a, b) = (SELECT ...)` assigns each listed column from
  the subquery's one row: once per updated row, or once per statement when
  the subquery is uncorrelated, so a volatile subquery runs once as on
  PostgreSQL. No row sets every listed column NULL; two rows are 21000; a
  column count that differs is 42601. It works through `UPDATE ... FROM`,
  over a set operation, `*` and VALUES. Fixes RP-031
  (tests/backlog/test_rust_postgres_gaps.py::test_update_multi_column_assignment_from_a_subquery).
- `RETURNING t.*` on an UPDATE or DELETE, and `RETURNING *` over
  `UPDATE ... FROM` / `DELETE ... USING`, return the columns PostgreSQL
  does: the target's, then each FROM item's. `t.*` had been refused, and
  `*` had silently returned the target's columns only. A `q.*` naming no
  relation is 42P01. Fixes RP-034
  (tests/backlog/test_rust_postgres_gaps.py::test_update_from_returning_target_star).
- A record keeps a `char(n)` field's padding when it prints
  (`("ab ",1)`), in `row_to_json` and in `array_agg`, and still compares it
  without the padding. Fixes RP-032 for whole-row values
  (tests/backlog/test_rust_postgres_gaps.py::test_record_text_keeps_char_padding).
- `pg_collation` lists 880 of PostgreSQL 15.19's 907 ICU collations, each
  checked to sort as PostgreSQL does. A `<tag>-x-icu` name that is not one
  of them is 42704, as on PostgreSQL. Fixes most of RP-005
  (tests/backlog/test_rp_triage.py::test_pg_collation_lists_locale_icu_collations).
- A table named after a built-in type (`create table tid (...)`) no
  longer shadows it: a bare `tid` is the built-in and the row type is
  `public.tid`, so `'(0,1)'::tid`, `row_to_json(x)`, `x::text` and
  `pg_typeof(x)` all answer as on PostgreSQL. Fixes RP-030
  (tests/backlog/test_rust_postgres_gaps.py::test_table_named_after_a_builtin_type).
- Most of RP-025 (tests/backlog/test_rust_postgres_gaps.py::test_typed_value_residue):
  `array[row(1)]` is a real `record[]`, and arrays of records compare;
  `regprocedure`, `regoper` and `regoperator` cover every built-in
  function and operator (tables dumped from PostgreSQL 15.19); a refused
  function-style cast (`int8(true)`) is 42883; ORDER BY, GROUP BY,
  DISTINCT, window clauses, aggregate ORDER BY / DISTINCT and set
  operations over a type with no ordering or equality operator (`point`,
  `json`, `xml`, ...) are refused with 42883 at PostgreSQL's position;
  `information_schema.columns.data_type` says `ARRAY` / `USER-DEFINED`;
  `path` compares by point count; `time` compared with `timetz` or
  `interval` is widened first; `macaddr = macaddr8` is 42725; and input-cast
  error positions follow PostgreSQL.
- `time(n)` / `timetz(n)` precision and `interval` field and precision
  modifiers are applied, in casts and to stored columns: a `time(1)`
  column kept `04:05:06.789`, an `interval day to minute` column kept its
  seconds, and `interval(1)` kept every digit. Rounding follows
  `AdjustTimeForTypmod` / `AdjustIntervalForTypmod` (`23:59:59.99::time(1)`
  is `24:00:00`).
- An identity column's options (`GENERATED ... AS IDENTITY (START 5
  INCREMENT 2)`) are its sequence's; they were ignored, so the column
  numbered from 1.
- `CREATE SEQUENCE ... AS smallint | integer` and `CACHE n` are obeyed: the
  type bounds the sequence (it had run past 32767) with PostgreSQL's
  out-of-range errors, `ALTER SEQUENCE ... AS` moves a bound left at the
  old type's limit, and a descending sequence's default minimum is the
  type's minimum. `ALTER SEQUENCE` refuses bounds that leave the start or
  the current value outside them, as PostgreSQL does. A serial's and an
  identity's sequence take their column's type.
- `format_type` prints every modifier as PostgreSQL does (`time(1) without
  time zone`, `interval day to minute`, `bit(3)`, `numeric(5,-2)`, an
  array's element modifier) and takes a `regtype`; `'_int4'::regtype`
  names the array type.
- `information_schema.columns` has PostgreSQL's 44 columns in its order
  (it had 23, in another), with `character_octet_length`,
  `numeric_precision_radix`, `datetime_precision`, `interval_type`,
  `udt_schema`, `dtd_identifier`, the `identity_*` options and
  `is_updatable` filled as `information_schema.sql` derives them.
- Without a `pg_hba.conf`, a login is proved before its database is looked
  up, as PostgreSQL does: a wrong password for a database that does not
  exist is `password authentication failed`, not `database ... does not
  exist`. Part of RP-013
  (tests/backlog/test_rust_postgres_gaps.py::test_wrong_password_checked_before_the_database).
- An untyped literal at a polymorphic parameter takes the type the call's
  typed argument fixes (`array_append('{1}', 2)` had been refused); the
  `anycompatible` family reads an all-untyped call as `text`
  (`array_cat('{1}', '{2}')`); and one that nothing fixes is PostgreSQL's
  42804 (`array_length('{1,2,3}', 1)`). Part of RP-015.
- Rows that tie under ORDER BY come out where PostgreSQL's quicksort
  leaves them (it is not stable from 7 rows up), including an aggregate's
  ORDER BY, and a LIMIT keeps PostgreSQL's bounded heap of OFFSET + LIMIT
  rows. Grouped output and DISTINCT ON keep their order for now. Fixes
  RP-028 (tests/backlog/test_rust_postgres_gaps.py::test_order_by_ties_as_postgres_quicksort_leaves_them).
- Under `standard_conforming_strings = off`, `N'...'` literals read their
  escapes as PostgreSQL's scanner does, and `backslash_quote` is a setting
  (on / off / safe_encoding) that refuses `\'` with 22P06 at the literal.
  Fixes RP-003 (tests/backlog/test_rp_triage.py::test_national_literal_under_nonconforming_strings,
  tests/backlog/test_rp_triage.py::test_backslash_quote_off_refuses_backslash_quote).
- A field read off a subquery or CTE record column keeps its type, a nested
  `ROW()` field comes back as a record, and the selections PostgreSQL
  refuses are refused with its codes (42703, 42809). Fixes RP-035
  (tests/backlog/test_rust_postgres_gaps.py::test_record_field_selection).
- The whole-row value of a subquery whose columns share a name reads its
  fields by position: `select q from (select 1, 'a' union select 1, 'a') q`
  is `(1,a)` (it was `(1,1)`). Fixes RP-033
  (tests/backlog/test_rust_postgres_gaps.py::test_whole_row_of_a_union_with_unnamed_columns).
- `ORDER BY ... USING <op>` in a query, an aggregate, a window and a set
  operation; an operator that is no ordering operator is 42809, one the
  type lacks 42883. Part of RP-025
  (tests/backlog/test_rust_postgres_gaps.py::test_typed_value_open).
- `ORDER BY` an expression over a `generate_series` source sorts by the
  expression (`order by x % 2` had come back in series order, a silent
  wrong answer).
- `FETCH FIRST n ROWS WITH TIES` returns the ties (it returned exactly n
  rows), in plain, grouped, set-operation, subquery and join queries, with
  PostgreSQL's 2201W for a NULL count.
- `SET x TO DEFAULT` / `SET x = DEFAULT` (and `SET LOCAL`) reset the setting,
  as RESET does; they had stored an empty value.
- `pg_settings.enumvals` lists each enum setting's values (it was NULL).
- With `standard_conforming_strings = off`, a typed literal (`int4'1'`,
  `date'2020-01-02'`) reads as one; it had failed with 42704.
- ORDER BY a range or multirange sorts as PostgreSQL's `range_cmp` /
  `multirange_cmp` do (empty first, then by bound), in a query, a window
  and over a VALUES column; it had sorted by the text, so `empty` came last
  and `[10,11)` before `[5,7)`. Fixes RP-037
  (tests/backlog/test_rust_postgres_gaps.py::test_order_by_a_range).
- Inside a correlated subquery, an outer column has its own type:
  `(select t.c = 'x' from s limit 1)` over a `char(3)` had compared an
  integer and answered false, and `pg_typeof` said `integer`.
- A subquery with no FROM that reads the outer row (`SET v = (SELECT v +
  1)`) runs as a correlated subquery instead of being refused.
- `UPDATE ... SET v = 1, v = 2` is 42601 `multiple assignments to same
  column`, as on PostgreSQL; it had applied the last.
- A row subquery that returns no row (`(select row(a, b) from t where
  false)`) is NULL instead of 42704 `type "record" does not exist`.
- `concat(t)`, `concat_ws` and `format('%s', t)` over a record print its
  text form, not the server's internal representation.
- `ROW(t.*)` is a row of t's columns, not a row holding t's row.
- An aggregate's `ORDER BY ... COLLATE "x"` naming no collation is 42704
  at the clause; it had sorted nothing.
- `default_text_search_config` is obeyed: `to_tsvector(text)`,
  `plainto_tsquery(text)` and the other one-argument forms, and
  `get_current_ts_config()`, use the session's configuration (they always
  used `english`). `SET` validates the name (22023) and keeps it as
  `pg_catalog.<name>`, as PostgreSQL shows it.
- An unknown text-search configuration, dictionary, role, schema or
  collation named in a string is reported at the literal. Part of RP-015
  (tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed).

#### Added

- The text-search catalogs `pg_ts_config`, `pg_ts_dict`, `pg_ts_parser`,
  `pg_ts_template` and `pg_ts_config_map`, with PostgreSQL 15.19's rows;
  `regconfig` and a new `regdictionary` resolve by name or oid
  (`3748::regconfig` is `simple`); and `ts_lexize`. Part of RP-015
  (tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed).
- The multi-byte encodings -- EUC_JP, EUC_CN, EUC_KR, EUC_TW, SJIS, BIG5,
  GBK, UHC, GB18030, JOHAB, SHIFT_JIS_2004 and EUC_JIS_2004 -- in
  `convert` / `convert_to` / `convert_from` and as a `client_encoding`,
  from PostgreSQL 15.19's own mappings (generated by
  `tools/pg_mbcs_tables.py`; GB18030's four-byte range by its rule), with
  PostgreSQL's 22021 / 22P05 errors. They had converted only their ASCII
  range. Part of RP-015.
- The geometric operators `point <> point`, `box # box` (the overlap, NULL
  without one), `box <^ box` / `>^`, the circle position tests `&<` `&>`
  `&<|` `|&>`, and `#` / `?#` over lines and `?#` over segments. Part of
  RP-015.
- `tsquery @> tsquery` / `<@` (and `tsq_mcontains` / `tsq_mcontained`).
  Part of RP-015.
- `VARIADIC` calls: an `ARRAY[...]` is spread into the variadic parameter
  (`concat(VARIADIC ARRAY['a', 'b'])`, `format('%s-%s', VARIADIC ...)`),
  and `json[b]_extract_path[_text]` / `jsonb_delete` take any array
  (`VARIADIC null::text[]` too). `jsonb_delete(j, k)` by name. Part of
  RP-015.
- `macaddr` / `macaddr8` `&`, `|` and `~`. Part of RP-015.
- The multirange operators `-|-`, `<<`, `>>`, `&<`, `&>` and `<` `<=` `>`
  `>=`, against a multirange or a range. Part of RP-015.
- The closest-point operators `##` (point to segment, box and line;
  segment to box and segment; line to segment) and `<->` between a line or
  box and a segment, transcribed from geo_ops.c. Part of RP-015.
- Distances to a segment or a box use geo_ops.c's own routines, so they
  hold at the float range (`lseg <-> point` had been NaN there); a
  `"char"` sorts by its byte (a high byte had sorted by its `\ooo` text);
  an UNLOGGED table is `relpersistence` `u` and not publishable. Part of
  RP-015.
- `pg_sequences`.
- `pg_relation_is_updatable`, `pg_column_is_updatable`,
  `pg_is_other_temp_schema` and `pg_settings_get_flags` (PostgreSQL
  15.19's flags for every setting). Part of RP-015.
- `acldefault`, `makeaclitem` and `aclexplode`, as PostgreSQL 15's acl.c
  answers them. Part of RP-015.
- The length-coercion functions by name, `pg_catalog."numeric"(x,
  typmod)` and the `varchar` / `bpchar` / `bit` / `varbit` / `time` /
  `timetz` / `timestamp` / `timestamptz` / `interval` forms; the
  non-explicit string and bit-string forms refuse an overlong value as an
  assignment does (22001 / 22026), and a modifier may be any expression.
  Part of RP-015.
- `to_regnamespace`, `to_regrole`, `to_regcollation`, `to_regproc` and
  `to_regprocedure`; a dotted schema or role name is 42602 `invalid name
  syntax`. `regcollation` columns are described with their own type oid.
- Corpora `multi_assign_update`, `returning_star`, `icu_collations`,
  `to_reg_functions`, `typed_value_residue`, `ts_catalogs`, `typmods`, `sequence_types`, `polymorphic_literals`, `acl_functions`, `relation_updatable`, `order_ties`,
  `string_syntax_settings`, `record_fields_using`, `fetch_ties_settings`
  `series_order_exprs`, `multibyte_encodings`, `geometric_ops_2`, `tsquery_containment`, `variadic_calls`, `macaddr_bitops`, `multirange_ops`, `range_order`, `geometric_closest` and `builtin_small`.
