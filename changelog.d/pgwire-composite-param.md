### Composite values bound as parameters over the Rust PG server's extended protocol

The Rust PostgreSQL server can now accept a user `CREATE TYPE ... AS (...)`
composite bound as a query PARAMETER. When psycopg's `register_composite` sends
a composite value it puts the type's OID in the `Parse` message and the value in
the `Bind` — but pgwire's `StoredStatement::parse` maps every parameter OID
through `Type::from_oid`, which only knows builtins and returns `None` for a
composite. The raw OID was gone before our parser ran, so a bound composite had
no resolvable type and every such query failed with `could not determine data
type of parameter $1`.

The fix vendors pgwire 0.40.7 into the tree (`crates/vendor/pgwire`, wired in
through `secantus-pgserver`'s own `[patch.crates-io]`) with a single-field
addition: `StoredStatement::parameter_oids` preserves the raw `Parse` OIDs
alongside the mapped types. The vendored copy is byte-identical to upstream but
for that one patch to `src/api/stmt.rs`; its licenses travel with it. The
server then resolves a parameter's raw OID against its own composite catalog,
gives the parameter a declared type (so `pg_typeof($1)` answers the type name),
and decodes the value into the same record BSON a `'(..)'::type` literal
produces — from the TEXT `(a,b)` form and the binary RECORD form alike,
recursing for a composite-typed field.

#### Added

- `crates/vendor/pgwire`: a locally-patched copy of pgwire 0.40.7 whose
  `StoredStatement` carries `parameter_oids: Vec<u32>` (the raw `Parse` OIDs),
  wired in via `secantus-pgserver`'s `[patch.crates-io]`.

#### Fixed

- `secantus-pgserver`: a user composite bound as a parameter now resolves its
  type from the raw `Parse` OID and decodes to the correct record value, so
  `register_composite` round-trips (text and binary) instead of failing with
  `could not determine data type of parameter $1`.
