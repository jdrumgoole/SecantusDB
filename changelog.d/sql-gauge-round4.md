### SQL server: oid/regtype, declared-parameter typing, and the full binary codec surface

Three parallel work streams off the psycopg gauge, landing together. The
`oid` type (26, arrays 1028) is now first-class — columns, casts, binary
codecs, `pg_type` rows — and `21::regtype` resolves an OID to its type name
the way Postgres does. Parameter typing got the same discipline on every
path: the OID a client declares in Parse now governs the value whether it
arrives in text or binary format, `'19.99'::numeric`-style scalar casts
convert instead of passing strings through, and Execute encodes DataRows
with the same column OIDs Describe reported (the mismatch fed text bytes to
clients parsing binary numerics). And the binary result/parameter codec
surface now covers what psycopg's full-type faker exercises: time, timetz,
interval, uuid, inet, cidr, macaddr, json, and every range and multirange
type — including new tstzrange/tstzmultirange registration, PG-exact
multirange rendering, JSON integers beyond int64, and Decimal128-safe
numeric handling at any width. psycopg's `test_leak` (the full-type
CRUD matrix) went from 72 failures to 72 passes; the six-file gauge subset
stands at 887 of 979 (91%), from 42% at the first external run.

#### Added

- `oid` type end-to-end; `N::regtype` OID resolution (42704 on unknown
  OIDs); `pg_typeof(x)::oid` resolves to the type's OID.
- Binary codecs (both directions) for time/timetz/interval/uuid/inet/cidr/
  macaddr/json and all range/multirange types; tstzrange/tstzmultirange
  types; `oid[]`/`json[]`/multirange array OIDs.

#### Fixed

- Execute now applies the same declared-parameter OID overrides to its
  DataRow encoding that Describe applies to RowDescription — the divergence
  sent int4/text bytes in fields announced as int2/numeric.
- Text-format parameters with a declared scalar OID convert to the native
  type (declared type governs, matching the binary twin; garbage raises
  22P02); scalar casts to int/float/numeric/bool convert with PG rounding
  semantics.
- Binary numeric survives values wider than Python's default 28-digit
  context (wide-context decode, context-free negate/abs); >34-digit
  numerics round into Decimal128 range instead of erroring on INSERT.
- Numeric/bytes/±inf parameters keep their types through statement binding
  (typed cast nodes / hex literals instead of bare string literals).
- Multirange text rendering drops the ", " separator Postgres doesn't
  print; daterange bounds render date-only; bool coercion of 'f'/'false'
  strings; JSON top-level scalars render as JSON.
