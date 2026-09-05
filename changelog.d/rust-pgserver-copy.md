### COPY FROM STDIN on the Rust PostgreSQL server

Bulk loading with `COPY table FROM STDIN` now works, in PostgreSQL's text
format. This is how `pgbench` populates its tables and how most bulk-load
tooling gets data in, so it matters out of proportion to the number of tests it
moves.

The escaping is the substance of the format rather than a detail of it. A null
value arrives as `\N`, which has to stay distinct from an empty string, and a
tab inside a value arrives escaped so that it is not mistaken for the separator
between fields. Data also arrives in chunks whose boundaries fall wherever the
client's buffer happened to end, including halfway through a row, so nothing can
be parsed until the client says it has finished sending.

`COPY TO STDOUT` was still refused when this was written; it landed shortly
afterwards, along with the CSV and binary formats — see the companion entry.

#### Added

- `COPY <table> [(columns)] FROM STDIN`, with `\N` nulls, escaped tabs,
  newlines and backslashes, an optional column list, and chunk boundaries that
  fall mid-row.
- PostgreSQL's `COPY n` completion tag, and its errors for a row with the wrong
  number of columns or a value that will not parse.
