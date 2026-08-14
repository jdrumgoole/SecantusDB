### The legacy COPY ... BINARY keyword selects the binary format

`COPY t FROM STDIN BINARY` — the pre-9.0 bare-keyword spelling that pgx
still emits — parses as a value-less COPY parameter, which the option
reader did not recognise. The format stayed "text", so the client's
PGCOPY binary stream was fed to the text parser and rejected with
`22021 invalid byte sequence for encoding "utf-8"`. The same applied to
`COPY t TO STDOUT BINARY`.

The bare `BINARY` keyword now selects the binary format on both COPY
directions, riding the existing PGCOPY parse/encode machinery that the
`WITH (FORMAT binary)` spelling already used.

#### Fixed

- `sql/engine.py`: `_copy_options` recognises the value-less `BINARY`
  COPY parameter (legacy pre-9.0 syntax) as `FORMAT binary` for both
  COPY FROM and COPY TO.
