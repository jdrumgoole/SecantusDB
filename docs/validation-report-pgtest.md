# pgtest wire-protocol conformance report

- SecantusDB (Python server) 0.6.0b12
- corpus + runner: cockroachdb/cockroach @ `e3bff5d92ac1` (`pkg/sql/pgwire/testdata/pgtest`, run by `pkg/testutils/pgtest` verbatim)
- generated: 2026-08-19 18:13 UTC

**49/66 files pass** (12 expected divergences, 0 unexpected failures, 5 skipped).

| file | result |
|---|---|
| `aborted_txn` | pass |
| `array` | pass |
| `as_of_system_time` | skip |
| `batch_stmt` | pass |
| `bind_and_resolve` | pass |
| `box2d` | expected divergence |
| `char` | expected divergence |
| `citext` | pass |
| `collated_string` | pass |
| `copy` | pass |
| `copy_file_upload` | pass |
| `data_type_size` | pass |
| `decimal` | pass |
| `enum` | pass |
| `errors` | pass |
| `execute` | pass |
| `float` | pass |
| `implicit_txn` | pass |
| `inet` | pass |
| `int2vector` | expected divergence |
| `int_size` | pass |
| `json` | pass |
| `json_array` | pass |
| `jsonpath` | expected divergence |
| `large_input` | pass |
| `ltree` | pass |
| `multiple_active_portals` | expected divergence |
| `multiple_active_portals/bind_to_an_existing_active_portal` | pass |
| `multiple_active_portals/different_portals_bind_to_the_same_statement` | pass |
| `multiple_active_portals/drop_table_when_there_are_dependent_active_portals` | pass |
| `multiple_active_portals/more_complicated_stmts` | pass |
| `multiple_active_portals/not_in_explicit_transaction` | pass |
| `multiple_active_portals/not_supported_statements` | pass |
| `multiple_active_portals/query_timeout` | expected divergence |
| `multiple_active_portals/select_from_individual_resources` | pass |
| `multiple_active_portals/select_from_same_table` | pass |
| `notice` | pass |
| `oid` | pass |
| `param_status` | pass |
| `parameter_description` | pass |
| `pgjdbc` | pass |
| `pgjdbc/158771` | pass |
| `pgvector` | expected divergence |
| `portals` | expected divergence |
| `portals_crbugs` | skip |
| `prepare` | pass |
| `prepared_stmt_invalidation` | pass |
| `procedure` | expected divergence |
| `read_committed` | skip |
| `row_description` | expected divergence |
| `schema_changes_implicit_txn` | pass |
| `schema_changes_implicit_txn/autocommit_for_bind` | pass |
| `schema_changes_implicit_txn/triggers` | pass |
| `set` | pass |
| `set_transaction` | skip |
| `show_commit_timestamp` | skip |
| `simple` | pass |
| `spatial` | expected divergence |
| `statement_hints_pausable_portal` | pass |
| `timezone` | pass |
| `tuple` | pass |
| `typing` | expected divergence |
| `unknown` | pass |
| `update_limit` | pass |
| `varbit` | pass |
| `void` | pass |

## Expected divergences

- `box2d` — PostGIS BOX2D — an extension type outside SecantusDB's core-PostgreSQL SQL scope. The ``::BOX2D`` cast falls through to a text passthrough, so the binary result is the text form rather than PostGIS's four-float8 box encoding. Out of scope, like GEOMETRY (see ``spatial``).
- `char` — char:250 pins TableOID=105 — crdb's deterministic descriptor id, with no ignore_table_oids directive on that stanza. Real PostgreSQL reports its own pg_class oid there too (installation-specific), so the stanza can't pass against any non-crdb server. Everything else in the file is green (oid-18 "char": casts, columns, params, 1-char truncation, NULL for empty/zero-byte, binary format).
- `int2vector` — int2vector:26 expects indoption={2} for a plain primary key — crdb's NULLS-FIRST pkey representation. Real PostgreSQL reports 0 (ASC, NULLS LAST), and so do we; matching crdb's 2 would corrupt SQLAlchemy index reflection. The BINARY int2vector encoding the stanza actually regression-tests (int2 array elements, elemoid 21 — crdb #111907 shipped int8 once) is implemented and correct.
- `jsonpath` — jsonpath:36/:76 expect crdb's BINARY jsonpath form — version byte + the SINGLE-QUOTED text ('$' -> 01272427). Real PostgreSQL's jsonpath_send emits the version byte + the canonical text WITHOUT outer quotes (0124), which is what we send. Everything else in the file is green (oid 4072, canonical $."abc" text, 42601 on an empty path, jsonb_path_query).
- `multiple_active_portals` — A CockroachDB pausable-portal test. Two of its subtests can't pass against a non-crdb server: (1) `query_timeout` expects a portal paged MaxRows:1 to emit N DataRows and THEN a 57014 statement-timeout on the next pull — that incremental behaviour needs LAZY per-row portal evaluation, but SecantusDB materialises a portal's result eagerly (statement_timeout IS enforced for simple / single-statement queries, just not row-by-row across a paged portal); (2) `interleave_with_unpausable_portal` pins crdb's `0A000 unimplemented: the statement for a pausable portal must be a read-only SELECT` error (with a go.crdb.dev hint) for interleaving a non-read-only portal — real PostgreSQL interleaves those fine and emits no such error. Every other subtest passes (select_from_individual_resources, select_from_same_table, bind_to_an_existing_active_portal, not_in_explicit_transaction, drop_table_when_there_are_dependent_active_portals, different_portals_bind_to_the_same_statement, more_complicated_stmts, not_supported_statements).
- `multiple_active_portals/query_timeout` — Expects a portal paged MaxRows:1 to emit several DataRows and then a 57014 statement-timeout on the next pull — needs LAZY per-row portal evaluation; SecantusDB materialises a portal's result eagerly, so statement_timeout (which IS enforced for simple / single-statement queries) can't page a portal row-by-row against the deadline.
- `pgvector` — The pgvector VECTOR type — an extension outside SecantusDB's core-PostgreSQL SQL scope. A ``VECTOR`` column is rejected with a faithful 0A000 (unsupported column type), which is correct emulation of a server without the extension installed, but the corpus expects a working vector type.
- `portals` — portals:1182 compares the CHECK-violation MESSAGE with keepErrMessage and pins crdb's wording ('failed to satisfy CHECK constraint (a > 1.0:::FLOAT8)'). We emit real PostgreSQL's ('new row for relation "t" violates check constraint "t_a_check"'), which is what psycopg/pgjdbc users parse — matching crdb would be a fidelity REGRESSION. Everything up to :1182 passes (1182 of 1550 lines, including PortalSuspended-on-exact-MaxRows and per-Execute row counts). NOTE: the stanzas after :1182 are therefore NOT exercised — they cover 34000 'unknown portal' (already implemented, slice 21) and 42P03 'cursor "p" already exists as portal' (NOT implemented). See tasks/backlog.md.
- `procedure` — procedure:68 pins the NoticeResponse's SOURCE-LOCATION fields to crdb's own Go internals — File="builtins.go", Routine="func401" — with no crdb_only marker. Those fields name the server's own source file and function, so they are unmatchable by any other implementation: real PostgreSQL 14 emits Routine=exec_stmt_raise, File=pl_exec.c (probed), and SecantusDB leaves them empty rather than fabricate a C source location it does not have. Everything the stanza actually regression-tests works: CREATE PROCEDURE ... LANGUAGE plpgsql, CALL p(), and the three RAISE NOTICE messages (foo / bar / baz) arriving in order with SQLSTATE 00000, followed by CommandComplete CALL.
- `row_description` — row_description:376 sends `SELECT 'foo'::STRING, 'bar'::STRING(2)` with NO crdb_only marker and expects crdb's STRING aliases (text/25 and varchar/1043 typmod 6, truncating 'bar' to 'ba'). Real PostgreSQL 14 rejects both casts outright — `ERROR: 42704 type "string" does not exist` (probed) — so the stanza can't pass against any non-crdb server, and matching crdb's varchar(2) truncation would diverge from PG. Everything before :376 is green: base-column identity across a JOIN and through a VIEW, char(n) blank padding on the wire, and attnum stability across ALTER COLUMN TYPE.
- `spatial` — PostGIS GEOMETRY/GEOGRAPHY — an extension type outside SecantusDB's core-PostgreSQL SQL scope (the surrogate models MongoDB, not PostGIS). A GEOMETRY value can't round-trip its EWKB binary form; an untyped binary GEOMETRY parameter now surfaces a faithful 22P03 rather than a generic internal error, but the type itself is not implemented.
- `typing` — typing's two non-crdb stanzas both use keepErrMessage and pin crdb's wording for a mixed-type comparison: 22023 'unsupported comparison operator: <varchar> = <uuid>' (and <varchar> = <bool>). Real PostgreSQL 14 raises 42883 'operator does not exist: character varying = uuid' (probed), which is what we now emit — matching crdb would be a fidelity REGRESSION, same as the portals file. The behaviour the stanzas actually regression-test (a DECLARED parameter type making the comparison unresolvable AT PARSE, rather than a predicate that silently matches nothing) is implemented and correct.
