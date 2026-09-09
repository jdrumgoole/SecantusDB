### The Rust PostgreSQL server passes psycopg's range and multirange suites

psycopg's `test_range.py` and `test_multirange.py` drive the whole range
family through the wire in every parameter format: lists of ranges bound as
`int4range[]`, untyped `Range(empty=True)` values that arrive as a single
binary flag byte, custom `CREATE TYPE ... AS RANGE` types registered through
`RangeInfo.fetch`, and a quoting sweep over every awkward bound character. The
Rust PG server failed 72 of those tests; it now fails one, a reserved-keyword
quoting nit that is not about ranges at all. Every behaviour was measured
against PostgreSQL 16 and matched exactly — values, canonical renderings,
error messages and SQLSTATEs.

Four things were wrong. A range-array parameter had no name for its oid, so a
literal beside it was compared as a string against an array. An untyped range
parameter bound in binary was read as text because nothing inferred its type
from the operand it was compared with (or from a `$1::int4range` cast).
Custom range types could be created but their values could not travel: the
constructor, the binary codec, and the schema-qualified name each resolved to
the wrong type or to `text`. And the literal parser and renderer disagreed
with PostgreSQL's `range_out` on doubled quotes, backslashes, non-ASCII
whitespace, and the always-exclusive infinite bound.

#### Added

- `secantus-pgplan`: `lower` / `upper` / `lower_inc` / `upper_inc` /
  `lower_inf` / `upper_inf` / `isempty` over ranges and multiranges, statically
  typed by the subtype (`lower(NULL::int4range)` describes as `integer`).
- `secantus-pgplan`: `AND` / `OR` / `NOT` in a FROM-less `SELECT`, three-valued,
  with PostgreSQL's `42804 argument of AND must be type boolean, not type
  integer` and `22P02` for an untyped literal that is not a boolean.
- `secantus-pgplan`: `infer_param_types` gives an undeclared parameter the type
  of a `$1::<range type>` cast, as it already did for a comparison operand.
- `secantus-pgserver`: binary decoding of custom range and multirange
  parameters (`binary_multirange` factored out of the builtin arm).

#### Fixed

- `secantus-pgplan`: a literal beside a range / multirange / range-array
  parameter is cast to the parameter's declared type
  (`'{empty,"(,)"}' = [Int4Range(...)]` was `text = text[]`, 42883).
- `secantus-pgserver`: an untyped binary range / multirange parameter takes its
  type from context before decoding (the `\x01` empty flag was read as text and
  every comparison answered False).
- `secantus-pgplan`: custom range constructors resolve their own type
  (`testrange('a', 'c')`, `testschema.testrange(1.5, 2.5)`), the one-argument
  form is the literal cast (`testrange('a')` is `22P02 malformed range literal`),
  and a schema-qualified type name keeps its schema (`'[1.5,2.5)'::testschema.testrange`
  reported the oid of `public.testrange`).
- `secantus-pgplan` `range.rs`: `""` inside a quoted bound is a literal quote;
  `"` and `\` are doubled on output; only C `isspace` characters are
  whitespace (U+0085 / U+00A0 are bound text); an infinite bound is exclusive
  in canonical form for every subtype (`'[,foo)'::testrange` is `(,foo)`).
- `secantus-pgserver`: `COPY ... FROM STDIN` stores ranges in canonical form
  (`{empty}` → `{}`, `[1,5]` → `[1,6)`), and `ascii(%s)` describes as `int4`.
