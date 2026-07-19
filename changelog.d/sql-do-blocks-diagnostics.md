### SQL server: DO blocks, backend termination, richer diagnostics, typed-param polish

A batch of protocol-conformance closers across the psycopg gauge's error,
connection, prepared-statement, cursor, and adapter suites.

`DO $$ … $$` blocks run through a minimal plpgsql interpreter: `RAISE
NOTICE`/`WARNING`/`INFO` surface as NoticeResponse messages (via the new
`SQLResult.notices`), `RAISE EXCEPTION` raises with its `USING ERRCODE`
(default P0001), and `EXECUTE format(…)` runs dynamic SQL whose errors keep
their real SQLSTATE. `pg_terminate_backend` / `pg_cancel_backend` close the
target connection through the live-session registry. ErrorResponse now
carries the optional diagnostic identity fields (schema/table/column/
constraint) and a statement position, so a CHECK violation reports its
constraint name and a name error renders the `LINE 1: …` caret context.

Typed parameters and introspection sharpen: `pg_prepared_statements`
reports each statement's original query text, real prepare time, and
regtype parameter names (with array typing); `DEALLOCATE ALL` clears the
extended-protocol registry; INSERT parameters infer their type from the
target column at Parse (so an untyped `%s` into a jsonb column types
correctly); `->`/`#>` type as jsonb and `->>`/`#>>` as text, and integer
JSON subscripts (`-> 1`) index arrays. `numeric` renders in plain
positional form (`1.1E+2` → `110`, matching `numeric_out`), `NaN = NaN`
holds, `generate_series(…)::int4` casts each element, the East-Asian client
encodings Python can convert are accepted, `format('%s/%I/%L', …)` works,
and `max_prepared_transactions` reports non-zero so drivers' 2PC probes
pass.

#### Added

- `pgwire.error_response` diagnostic fields + statement position;
  `notice_response` full severity/sqlstate.
- `errors.SQLError` carries `diag` / `position`.
- `SQLResult.notices`, rendered as NoticeResponse in both protocols.
- `TableDef.temp` (CREATE TEMP TABLE — reflected in error schema).
