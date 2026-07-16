### SQL server: range and multirange parameters become first-class values

Range-typed parameters used to arrive as raw text and never become range
values: `select 'empty'::int4range = %s` with a psycopg `Range` parameter
silently compared a subdocument against a string and returned false. A
parameter declared with a range or multirange OID (or their array forms) now
travels as tagged text and substitutes as a `::type` cast, so the existing
cast coercion turns it into the structured value. Array casts
(`'{empty,"[1,3)"}'::int4range[]`) coerce their elements, untyped literals
compared against a range value take the range's type (Postgres' context
inference), and `range::text` renders the `[a,b)` literal.

Equality itself also got Postgres semantics: range bounds store in the
subtype's canonical form regardless of construction path (a
`daterange(date, date)` constructor bound now matches the text cast's bound;
`numrange` bounds unify int / Decimal / Decimal128), and comparisons go
through a representation-independent canonical identity. psycopg's range and
multirange suites drop from 149 failing + 31 errors to 10 + 31 — the
remainder being untyped binary parameters (psycopg dumps a bound-less
`Range(empty=True)` with OID 0 in binary; needs Parse-time parameter-type
inference) and `CREATE TYPE … AS RANGE` (both recorded in `tasks/backlog.md`).

#### Added

- `ranges.canonical` / `canonical_multirange`: representation-independent
  range identity used by comparisons.
- `typemap.TaggedText`: the typed-parameter carrier for range/multirange
  (and array-of-range) declared OIDs.

#### Fixed

- `pgextended.py`: text and binary range/multirange parameters (and binary
  arrays of them) substitute as typed casts; `ParameterDescription` resolves
  an undeclared parameter to `text` like Postgres' parse analysis instead of
  echoing 0.
- `ranges.make_range`: bounds coerce to the subtype's canonical storage form.
- `scalar.py`: untyped-literal context coercion against range values;
  `::range[]` element coercion; `range::text` rendering.
