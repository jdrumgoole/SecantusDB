### COPY BINARY on the Rust PostgreSQL server encodes real binary, not text bytes

The Rust `secantusd-pg` server accepted `COPY ... (FORMAT BINARY)` in both
directions, but on the way *out* it only knew how to write the four types whose
value happens to encode the same in text and binary — `int4`, `int8`, `float8`,
`bool`, `text`. Every other column was handed to the binary encoder as its
PostgreSQL *text* rendering, so a `smallint` went out four bytes wide, a
`numeric` / `date` / `time` / `timestamp` / `bytea` / array column went out as
ASCII digits behind a binary length prefix, and a strict client reading the
stream back mis-parsed it (psycopg's binary array loader read a garbage
dimension count and raised `DataError`). A round-trip only survived because our
own decoder read the same wrong bytes back.

COPY binary out now goes through the exact `encode_binary` codec the `SELECT`
binary path uses — a `DataRowEncoder` over a binary-format schema produces the
`[i32 len][bytes]` layout a COPY field wants byte-for-byte — extended with the
three temporal encoders (`date` as an i32 day count, `time` as i64 microseconds
since midnight, `timestamp` / `timestamptz` as i64 microseconds since 2000). A
timestamp column's hidden sub-millisecond companion is folded back before
encoding, so `.ffffff` fractional seconds survive the round-trip. Measured
against PostgreSQL 16 as the oracle, every common scalar and array type
(`int2`/`int4`/`int8`, `float4`/`float8`, `bool`, `text`/`varchar`, `numeric`,
`date`, `time`, `timestamp`, `timestamptz`, `bytea`, and `int4[]` / `float8[]` /
`text[]`) now emits bytes identical to the real server's.

#### Fixed

- `crates/secantus-pgserver`: `COPY ... TO STDOUT (FORMAT BINARY)` now routes
  every field through `encode_binary` (the shared SELECT-binary codec) instead
  of a text fallback, fixing the wire form for `int2`, `float4`, `numeric`,
  `date`, `time`, `timestamp`, `timestamptz`, `bytea`, and array columns. The
  psycopg `test_read_rows[*-1]` binary cases (a `float8[]` COPY read) now pass.
- `crates/secantus-pgserver`: a `timestamp` / `timestamptz` column's `__us_`
  sub-millisecond companion is reassembled into the value COPY reads, so
  microsecond-precision timestamps no longer lose their last three digits.

#### Added

- `crates/secantus-pgplan`: `date_to_pg_days`, `time_to_pg_micros`, and
  `timestamp_bson_to_pg_micros` — the inverses of the existing
  `render_date_from_pg_days` / `render_time_from_micros` /
  `render_timestamp_from_pg_micros` decoders, used by the binary COPY encoder.
