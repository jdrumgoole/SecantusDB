### Composite VALUES round-trip on the Rust PostgreSQL server

With `CompositeInfo.fetch` already reading the catalog, the Rust PostgreSQL
server can now round-trip a composite VALUE on the wire. A `'(1,x)'::testcomp`
text cast and a `row(1,'x')::testcomp` record cast — the first used to answer
`invalid input value for enum testcomp`, the second `a record cast to testcomp
is not supported yet` — both parse into a composite datum whose result column
carries the composite type's own oid, so psycopg's `register_composite` loader
fires and hands back the registered namedtuple. A composite param cast on the
wire (`%s::testcomp`), an `INSERT`/`SELECT` through a column of the composite
type, and an array of composites all round-trip, with field escaping (NULL, the
empty string, and fields carrying commas, quotes, backslashes, parentheses or
whitespace) matching PostgreSQL exactly across all 255 single-byte characters.

On psycopg's own composite test suite this moves the Rust server from 25 to 45
of 79 passing. The remaining families — the composite BINARY wire format, a
composite parameter typed with its own oid (rather than explicitly cast),
`pg_typeof` / field access of such a parameter, and per-element loading of an
array of composites — are tracked in `tasks/backlog.md`.

#### Added

- `secantus-pgplan`: a composite arm in `cast_value` that parses PostgreSQL
  composite TEXT (`parse_composite_text`) or coerces a record's fields into a
  composite value (`composite_value`), plus a `PLAN_USER_COMPOSITES` thread-local
  (`set_user_composites` / `user_composite` / `user_composite_oid`) carrying each
  composite's field metadata to the planner.
- `secantus-pgserver`: `install_user_types` now pushes composite field metadata,
  and `user_wire_type` reports a composite result column under its own oid so a
  client that ran `register_composite` decodes it.

#### Fixed

- `secantus-pgplan`: an array element that is a record / composite now renders as
  its `(...)` text instead of leaking Rust's `{:?}` debug form
  (`{"Document({...})"}`) into the array literal.
