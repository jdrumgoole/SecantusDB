### The Rust PostgreSQL server passes psycopg's prepared-statement, uuid and string suites

psycopg's `tests/test_prepared.py`, `tests/types/test_uuid.py` and
`tests/types/test_string.py` went from 45 failures to none against the Rust PG
server (`secantusd-pg`), every change measured against PostgreSQL 16 first.
`pg_prepared_statements` now lists the connection's named protocol-level
statements — statement text, `prepare_time`, `parameter_types` and
`result_types` as regtype names, and the generic / custom plan counts psycopg's
tests read — and a protocol `Close`, `DEALLOCATE <name>` or `DEALLOCATE ALL`
removes the row. A COPY TO STDOUT that fails mid-stream no longer follows its
error with a CopyFail, which had left the client believing a COPY was still in
progress ("you cannot mix COPY with other operations") for the rest of the
connection.

`uuid_in` accepts every spelling PostgreSQL does (braces, upper case, a hyphen
after any group of four hex digits), a text parameter bound to a `$n::uuid` is
canonicalised, and `uuid`, `bytea`, `inet`, `cidr` and their arrays are sent in
binary when the client asks for it — psycopg decodes a whole row with the first
column's format, so a single text column in an otherwise binary row was
undecodable. A NUL byte inside a binary text parameter is the 22021
PostgreSQL gives it, and the `any`-typed builtins (`concat`, `concat_ws`,
`format`, `num_nulls`, `json_build_*`, …) refuse an untyped parameter with
42P18, naming the parameter PostgreSQL names. `UPDATE … SET col = <expression
over the row>` — `num = num * 2`, `s = upper(s)`, `num = coalesce(num, 0)` —
evaluates per matched row, and ROLLBACK TO a savepoint on a store with no
committed tables now undoes a CREATE TYPE issued after it.

#### Fixed

- `crates/secantus-pgserver`: a `pg_prepared_statements` registry of named
  Parse statements (name, statement, `prepare_time` timestamptz,
  `parameter_types` / `result_types` `regtype[]`, `from_sql`, `generic_plans`,
  `custom_plans`), maintained by `on_parse` / `on_close`, `DEALLOCATE <name>`
  (26000 when missing) and `DEALLOCATE ALL`; `NOTIFY` completes with its tag.
- `crates/secantus-pgserver`: Describe of a zero-oid Parse sizes the parameter
  list from the highest `$n` in the text, so `select $1::uuid` describes
  instead of "there is no parameter $1".
- `crates/vendor/pgwire`: a COPY OUT / COPY BOTH handler error is returned as
  the error alone — no trailing CopyFail (local patch in `api/copy.rs`).
- `crates/secantus-pgplan`: `parse_uuid` accepts a hyphen after any group of
  four hex digits and the brace form; `uuid_to_wire` for the binary encoding;
  `catalog_param_types` / `max_param_number` / `static_text_type` for the
  registry; `refuse_untyped_any_args` (42P18, skipping `format`'s and
  `concat_ws`'s leading `text` argument).
- `crates/secantus-pgserver`: `uuid`, `bytea`, `inet`, `cidr` and their array
  types are binary-encodable; an `inet[]` / `cidr[]` in text renders each
  element through `inet_out` (a host address drops its full mask); a text
  parameter for a `uuid` target casts through `uuid_in`; a NUL byte in a binary
  text-family parameter is 22021.
- `crates/secantus-pgplan` / `-pgserver`: `UPDATE … SET` values that read
  the row are planned as row expressions (`Update::set_exprs`) and evaluated
  per matched row before the first write (`update_row_sets`).
- `crates/secantus-storage`: `collection_exists` reads on the active user
  transaction's session, so a savepoint restore sees a collection the same
  transaction created.
