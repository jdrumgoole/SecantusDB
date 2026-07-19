### SQL server: binary COPY, Parse-time inference, full array-literal grammar

`COPY … (FORMAT binary)` works in both directions: COPY OUT emits the PGCOPY
stream (signature/flags header bundled with the first row the way real PG
frames it, one CopyData per row, int16 -1 trailer) with each field encoded by
its column type through the existing binary result encoders, and COPY IN
parses the same layout, decoding fields by the target column's type.

Untyped parameters get real Parse-analysis type inference — a client that
binds a value in binary format with no declared type (psycopg's
`Range(empty=True)` dump sends OID 0) takes its type from the AST (a cast on
the parameter, a cast or range-constructor operand it's compared with) — and
an untyped parameter fed straight to a VARIADIC "any" function (`concat`)
raises 42P18 like real Postgres.

The array machinery reaches PG's full literal grammar: nested `{{…}}`
multi-dimensional arrays parse, render, and encode/decode in binary
(row-major with per-dimension headers), `[l:u]=` bounds prefixes parse,
`box[]`'s `;` delimiter is honoured both ways, `int[][][]` collapses to the
one array type like PG, `'{a,b}'::text[]` casts materialise real lists (so
subscripting and `unnest` work), array concatenation `||` concatenates
lists, and `= any('{1,2}')` accepts array-literal operands. E-string
literals are now decoded by the engine *before* sqlglot parses (sqlglot's
half-decoding was lossy for `E'\\x5c'`), `json` and `jsonb` columns carry
their distinct OIDs (114/3802 — plain json's binary form has no version
byte), table row types appear in `pg_type` (typtype `c`, with `typarray`,
resolvable via regtype so psycopg's `TypeInfo.fetch(conn, "<table>")`
works), and the `name`/`aclitem` types exist with their real OID pairs.

Range text literals follow PG's quoting rules exactly: quoted bound tokens
with `""`/`\X` escapes parse, embedded quotes/backslashes double on render,
ASCII-only whitespace trimming (Python's unicode-aware `.strip()` corrupted
NBSP/NEL bounds), and a user-declared range constructor result describes
with its minted OID. psycopg's range, multirange, json, array, and string
suites all pass.

#### Added

- `GROUP BY 1` positional references resolve to the select-list expression
  (42P10 when out of range); computed GROUP BY keys beyond the aggregation
  engine's operators (`GROUP BY col = ascii(x)`, `substr(…)`) evaluate
  per-doc in Python before the pipeline, typed from the source expression.
- bytea params substitute through `::bytea` / `::bytea[]` casts so equality
  against computed bytea values compares bytes (text-format arrays decode at
  Bind); `(x::box[])::text` renders with the element's rules.

#### Fixed

- Multi-statement `COPY` strings raise 42601 (ProgrammingError), bytea cast
  failures 22P02 (was XX000), and the binary array encoder covers
  varchar[]/bpchar[] (1015/1014).
