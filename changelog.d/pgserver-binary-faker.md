### The Rust PG server answers every type psycopg draws in binary, byte-for-byte

psycopg reads every column of a row in the format of column 0, so a result set
that mixes one text-only type in with binary ones makes the client run text
loaders over binary payloads. The psycopg gauge's randomised "faker" tests —
`test_leak`, `test_copy_to_leaks`, `test_random` and their async twins — draw a
random assortment of types per run and were failing on roughly ninety
parameterisations for exactly that reason. The Rust PG server now encodes the
whole faker family in binary — the date / time / timetz / timestamp /
timestamptz family (including `24:00:00`, years past 9999 and the BC era),
`interval`, every range and multirange, `json` / `jsonb` and their arrays, and
empty arrays — and each layout was pinned against PostgreSQL 16 by comparing
the raw bytes, not by asking whether the client could decode them. A dozen
smaller things fell out of the same runs: a `timestamptz` result in a named
session zone (`Europe/Rome`) now carries the right offset; `set_config` reports
the zone the way `SET` does; `jsonb` renders Unicode escapes as PostgreSQL
does; COPY inside a transaction leaves the session in it, fills omitted
`serial` / default columns, and parses array literals; and an empty-multirange
binary parameter with no declared type is typed from the column it is inserted
into. The remaining faker failures are all the documented 34-significant-digit
`numeric` limit.

#### Fixed

- `secantus-pgserver`: binary result encoders for the datetime family,
  `interval`, ranges, multiranges, `json`/`jsonb` and their arrays, and empty
  arrays, each pinned to PostgreSQL 16's bytes
  (`test_binary_results_cover_every_faker_type`).
- `secantus-pgplan`: `timestamp_text_to_pg_micros` handles wide years, the BC
  era and an explicit offset; `time_to_pg_micros` accepts `24:00`; a
  `timestamptz` result is rendered in the session's named zone
  (`utc_text_in_zone`); datetime array text is PostgreSQL's, not a debug dump.
- `secantus-pgplan`: `catalog_param_types` infers an INSERT's parameter type
  by column position when the statement has no column list (`ColumnRef`), so
  an untyped binary parameter is typed from its target column either way.
- `secantus-pgserver`: COPY inside a transaction keeps `INTRANS`; COPY with a
  column list fills the omitted columns from their defaults and sequences;
  COPY FROM parses array literals; `set_config('TimeZone', ...)` reports the
  new zone; `jsonb` text uses PostgreSQL's Unicode escaping.
- vendored `pgwire`: `put_cstring` stops at the first NUL and `ErrorResponse`
  / `NoticeResponse` length their fields the same way, so an error message
  that quotes a binary parameter no longer drops the connection with "message
  contents do not agree with length".
