### `getProcedureColumns` could not describe a function's arguments

`pg_proc.proargmodes` and `proallargtypes` were always NULL, so nothing recorded
that a parameter was `OUT` or `INOUT`. Around that sat four more defects on the
same rows, each of which independently broke a client reading a routine's shape:

- `proargtypes` listed **every** parameter, but it is the function's call
  signature — an `OUT`-only parameter is excluded. `f3(IN a int, INOUT b
  varchar, OUT c timestamptz)` advertised a three-argument signature where
  PostgreSQL 14 records `23 1043`.
- `prorettype` was `2278` (void, an oid this catalog does not even define) for
  any function whose return type came from its outputs. With one output column
  it is that column's type; with two or more it is `2249` (`record`).
- `RETURNS TABLE (...)` recorded nothing about its columns. PostgreSQL lists
  them in the same three arrays with mode `t`.
- `RETURNS <composite>` also reported void: a user type has no storage tag, so
  the name is now recorded at CREATE and resolved to the type's oid at
  reflection time, where the catalog is in scope.

`proallargtypes` is declared `oid[]` rather than `text[]` because pgjdbc casts
that array to `Long[]` — a text array throws `ClassCastException` inside the
driver before any assertion runs.

#### Fixed

- `proargmodes` / `proallargtypes` are populated when any parameter is not a
  plain `IN`, and stay NULL otherwise — PostgreSQL's own rule, which the driver
  switches on.
- `proargtypes` is the call signature, excluding `OUT`-only parameters.
- `prorettype` is derived from output parameters, `RETURNS TABLE` columns, or a
  named composite type.
- `pg_type.typtype` reports `p` for `record` and `m` for a multirange; both read
  `b`. Not cosmetic — pgjdbc decides whether to emit a leading `returnValue`
  row by switching on `typtype`, so `record` reading `b` gave a function with
  OUT parameters a spurious extra row. That one surfaced only **after** the
  argmodes were already correct, which is why it is in this change.

All values measured against PostgreSQL 14 on 2026-09-20.

Measured on pgjdbc's `DatabaseMetaDataTest` (194 tests): **33 failures → 25**,
20 distinct → 16, no regressions. `funcWithDirection`, `funcReturningComposite`,
`funcReturningTable` and `droppedColumns` go green.

One narrowing worth recording: sqlglot parses `RETURNS refcursor` as a
USERDEFINED type name — the same shape as `RETURNS <composite>` — even though
`type_tag_for_sql` resolves it perfectly well. The first version of the
composite-return fix claimed every USERDEFINED name and so dropped refcursor's
tag, making `SELECT getref()` describe its column as `text` (25) instead of
`refcursor` (1790). The discriminator is whether the type resolves to a storage
tag, not what sqlglot called it. Caught by the full suite, not by the gauge.
