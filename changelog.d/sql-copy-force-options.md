### COPY learns FORCE_QUOTE, FORCE_NULL and FORCE_NOT_NULL

#### Added

- `FORCE_QUOTE (col, ...)` and `FORCE_QUOTE *` on `COPY ... TO` in CSV mode
  quote the named columns even where quoting is not required. A NULL is not
  force-quoted, which is what keeps it distinguishable from the empty string
  under `FORCE_QUOTE *`, and the `HEADER` line is not force-quoted either.
- `FORCE_NULL (col, ...)` on `COPY ... FROM` in CSV mode reads a quoted empty
  field as NULL; `FORCE_NOT_NULL (col, ...)` reads an unquoted empty field as
  the empty string rather than NULL.

Each option is valid in exactly one direction, and they disagree about which:
`FORCE_QUOTE` is `COPY TO` only, `FORCE_NULL` and `FORCE_NOT_NULL` are
`COPY FROM` only. All three are CSV-only, and an unknown column raises `42703`.

sqlglot cannot parse any of them — it raises a hard error on the whole
statement — so they are lifted out of the SQL text before parsing and
re-attached to the syntax tree. That rewrite applies only to a statement
beginning with `COPY`, and never inside a string literal.

#### Fixed

- `DELIMITER '"'` in CSV mode is refused with `22023 COPY delimiter and quote
  must be different`, as PostgreSQL does. It was accepted, and produced CSV
  that cannot be parsed back.
