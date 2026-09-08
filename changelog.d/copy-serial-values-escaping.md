### The Rust PostgreSQL server closes three COPY gaps

`secantusd-pg` (the Rust PostgreSQL server) gained three fixes that psycopg's
own `test_copy.py` suite was catching, taking that gauge from 54 failures to 24
against a live PostgreSQL 14 oracle.

A `serial` column now stores integers. `serial` is a PostgreSQL pseudo-type
that resolves to `int4`; the catalog was keeping it typed `serial`, so a COPY
field parsed as text and `40010` came back as the string `"40010"` — the shape
the whole `test_copy_in_*` cluster hit. `smallserial`/`serial2`,
`serial`/`serial4`, and `bigserial`/`serial8` now normalise to `int2`/`int4`/
`int8` at `CREATE TABLE` time. (The implicit sequence default a real `serial`
carries is still a separate, unimplemented feature.)

`COPY (VALUES ...) TO STDOUT` reads every row. A bare multi-row `VALUES` list
was planned through the single-row constant path and produced no rows at all, so
`copy (values (1),(2)) to stdout` returned an empty body; it is now planned as a
`ValuesConstant` that both COPY and a directly-executed `VALUES` query read.

Text COPY stopped corrupting control characters. The unescaper handled only
`\t \n \r \\`, so PostgreSQL's `\b`/`\f`/`\v` escapes were read back as literal
`b`/`f`/`v` — and a second, redundant unescape pass also halved an escaped
backslash twice, dropping it from a value like `\end`. The path now decodes the
full `\b \f \n \r \t \v \\` set plus octal (`\ooo`) and hex (`\xHH`) byte
escapes, over bytes, once. Silent data loss for any text carrying those bytes.

#### Fixed

- `secantus-pgplan`: `serial`/`bigserial`/`smallserial` (and the `serialN`
  aliases) normalise to their integer type at `CREATE TABLE` time.
- `secantus-pgplan`: a bare `VALUES (...), (...)` query plans as a new
  `ValuesConstant` statement instead of an empty single-row constant.
- `secantus-pgserver`: text COPY unescaping is complete (`\b \f \v`, octal, hex)
  and no longer runs twice, ending control-character and backslash corruption on
  COPY FROM; the text COPY-TO encoder escapes `\b \f \v` to match.
