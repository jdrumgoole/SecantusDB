### The jsonpath type

`::jsonpath` casts now produce a real jsonpath value: oid 4072 on the wire,
PostgreSQL's canonical text form (`$.abc` renders `$."abc"`, subscripts and
filters re-render canonically), a 42601 syntax error on an empty path, and
the binary format PostgreSQL sends — a version byte followed by the
canonical text. `jsonb_path_query` accepts a string first argument by
coercing it to jsonb like PG's implicit cast, and unaliased function-call
output columns are now named after the function (`SELECT jsonb_path_query(…)`
yields a column named `jsonb_path_query`, PG's rule) instead of `?column?`.
The pgtest `jsonpath` corpus file pins everything except its two binary
stanzas, which expect CockroachDB's single-quoted binary wrapping where
PostgreSQL (and SecantusDB) send the unquoted text — recorded as an
expected divergence.

#### Added
- The `jsonpath` type: canonical text rendering, oid 4072, binary codec.

#### Fixed
- `jsonb_path_query('{"a": true}', '$.a')` returned NULL — the string
  document never coerced to jsonb.
- Unaliased function-call columns were named `?column?`.
