### The Rust PostgreSQL server runs `DO` blocks and speaks more of the wire

psycopg's own test suite exercises corners of the protocol that no application
reaches on purpose: a `DO $$ ... $$` block that raises a notice, a connection
set to `latin9` reading an error message with a euro sign in it, a `ROW(...)`
fetched in binary, an enum array sent as bytes. Each of those now answers the
way PostgreSQL 16 does, measured against it. The largest piece is a small
executor for inline `plpgsql` blocks — `RAISE` at every level with `USING`
options and `%` formatting, `PERFORM`, `EXECUTE`, `NULL` and plain SQL — with
the context, position and `internal_query` fields a client sees on failure.
It is deliberately a subset: variables, control flow and `EXCEPTION` handlers
are still refused with `0A000` rather than half-run.

Around it, a cluster of smaller fidelity fixes: an anonymous record's binary
form now carries each field's real type (an untyped literal is `unknown`, a
cast one its cast type), `quote_ident` and `regtype` quote reserved keywords,
`format()` arrives with its `%s` / `%I` / `%L` specifiers, a `WHERE` clause
works over `generate_series`, `inet` values drop their host mask on `COPY TO`,
and `oid`, `oid[]` and enum-array parameters resolve to their own types in both
text and binary.

#### Fixed

- `secantus-pgserver`: `DO [LANGUAGE plpgsql] $$ ... $$` inline blocks
  (`plpgsql_do.rs` parser + `do_block.rs` executor). `RAISE` notices reach the
  client as `NoticeResponse` with `PL/pgSQL function inline_code_block line N
  at RAISE` context; `RAISE EXCEPTION` carries `P0001` / a named condition /
  an `errcode`, plus `detail` / `hint` / `column` / `constraint` / `datatype` /
  `table` / `schema`; an error inside `PERFORM` stacks the SQL statement under
  the block frame, and one inside `EXECUTE` carries the executed text in
  `internal_query` / `internal_position`; a bad body is `42601` positioned
  inside the statement; an unknown condition name is `42704` with the
  compilation context; `LANGUAGE sql` is `0A000`.
- `secantus-pgserver` / `pgwire`: every error and notice now carries the `V`
  (`severity_nonlocalized`) field; `42P01` for an undefined table carries the
  `P` position of the relation's first mention; error messages are re-encoded
  in the client's encoding (`latin9` reads `bad €`).
- `secantus-pgplan`: `ROW(...)` records each field's type, so a binary
  anonymous-record result is byte-identical to PostgreSQL (`unknown` 705 for a
  bare literal, the cast type otherwise); `-'NaN'::numeric` and the infinities
  negate; `regtype` and `quote_ident` quote reserved keywords (`"order"`);
  `format()` with `%s` / `%I` / `%L` / `%%` / positional `%n$`, NULL handling,
  and the `22023` / `22004` errors (with PostgreSQL's hint); `concat(true)` is
  `t`; a `WHERE` over `generate_series` — constant or on the series column —
  filters the rows, and a non-boolean constant is `42804`.
- `secantus-pgserver`: `COPY ... TO STDOUT` renders `inet` without a `/32` /
  `/128` host mask (`::ffff:102:300/128` → `::ffff:1.2.3.0`) while `cidr`
  keeps its mask; `COPY FROM` stores numeric text at its written scale.
- `secantus-pgserver`: an `Oid` parameter keeps type 26 (`pg_typeof` is
  `oid`), an `oid[]` sent as text comes back binary as 1028; a parameter
  typed with a user enum's oid is checked against its labels (`22P02 invalid
  input value for enum ...` with the `unnamed portal parameter $1` context);
  enum ARRAY parameters parse in text and binary, and a binary enum-array
  result carries the array oid with each element as its label.
