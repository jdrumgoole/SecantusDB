### COPY in every format, and where NULL hides

`COPY` now works in all three of PostgreSQL's formats, in both directions, and
from a query as well as a table.

The three formats differ in more than punctuation. They differ in how they spell
NULL, and that is where every bug in this work turned out to live. Text writes
`\N` for NULL, so an empty field is an empty string. CSV writes NULL as an
*unquoted* empty field, which forces it to quote the empty string as `""` to
keep the two apart. Binary writes a length of minus one, where an empty string
is a length of zero. Each of those distinctions was broken at some point while
writing this, and each broke the same way — NULL and empty string became
indistinguishable, which is silent, survives a round trip in one format, and
corrupts data in another.

The binary case is worth recording because the cause was ordinary and easy to
repeat: a catch-all match arm sat above the NULL arm and swallowed it, rendering
NULL as text and so writing a zero-length field. Match arms are tried in order,
and a catch-all has to come after every case it must not absorb.

`COPY (query) TO STDOUT` reuses the ordinary query path rather than reading rows
a second way, so anything a select can do — ordering, limits, a generated series
— works inside a COPY too. That includes a query with no `FROM` at all: clients
use `copy (select 1) to stdout` to check how a server reports a bad query, so
refusing it failed a whole file of tests that were not about COPY.

Binary input decodes each value with the same code that decodes a bound binary
parameter. The bytes on the wire are identical, so a second implementation could
only drift from the first.

#### Added

- `COPY ... TO STDOUT` and `FROM STDIN` in text, CSV and binary formats.
- `COPY (query) TO STDOUT`, including queries with no `FROM`.

#### Fixed

- `COPY ... TO STDOUT` in text format wrote an empty field for NULL instead of
  `\N`, losing the distinction between NULL and the empty string.
