### The Rust PostgreSQL server: composite keys named like extended JSON, pg_lsn, and eight silent wrong answers

A composite primary key whose columns are named like MongoDB extended JSON
(`"$date"`, `"$regex"`, `"$binary"`, `"$ref"`) broke the table that held it:
a SELECT read the key back as NULL, every read failed, or an UPDATE changed
nothing and said it had. Rows were decoded through serde, which reads such a
sub-document as the value it stands for, and found again by a bare
`{_id: key}` filter, which reads it as query operators. `json` and `jsonb`
values were never affected: they are stored as text.

The built-in function sweep's eight silent wrong answers are fixed, and
measuring them found four more: a `pg_lsn` column compared with a literal
matched no rows, and `ts_headline` ignored its options and marked up a
document's keys.

#### Fixed

- **Composite keys named like extended JSON** read, update, upsert, change
  key and cascade through a foreign key as PostgreSQL 15.19 does. Rows are
  decoded literally (`secantus_core::bsondoc::from_bytes`), a row is found
  again by `{_id: {$eq: key}}`, and a foreign key's checks name a `"$x"`
  column through `$getField`.
- **`pg_lsn` arithmetic** answers a `pg_lsn`: `'0/16'::pg_lsn + 1.5` is
  `0/18`, rounded half away from zero and then range-checked (22023), with
  PostgreSQL's errors for NaN and infinity and 42883 for an operator it does
  not have. An untyped literal beside a `pg_lsn` is one (`l = '0/16'`, which
  matched nothing over a table), except as the offset of `+`; and
  `set l = l + 1` no longer fails.
- **`lseg ?# lseg`, `lseg # lseg` and `line <-> point`** are transcribed
  from `geo_ops.c`: two segments on one line do not intersect, and the
  distance is `pg_hypot`'s, last digit included.
- **`ts_headline` over `json` / `jsonb`** marks up the string values only
  and answers the document, of its own type; `json` comes back with no
  whitespace. Its options are read by `deserialize_deflist`'s rules:
  `StartSel` and `StopSel` apply, an unknown name is 22023, and the options
  that choose which words are shown are refused by name (0A000) rather than
  ignored.
- **`"char"`** converts to and from `int4` as a byte (`"char"(65)` is `A`,
  `200` is 22003 out of range), reads `'\310'` and the first byte of
  non-ASCII text, and prints a high-bit byte as `\ooo`.
- **`pg_relation_is_publishable`** is true only for a user table that is
  not temporary, and NULL for an oid that names no relation.
- **`array_agg` over arrays** refuses a NULL array (22004), an empty first
  array and arrays of different shapes (2202E), where it failed on encoding
  or answered a ragged array.
- **A built-in called by its operator's name** (`int4pl`, `textlen`,
  `dexp`) refuses a literal PostgreSQL would not coerce to its parameter
  (42883), where it computed an answer.
- **`tools/probes/pg_login_options.py`** counts only its own session in
  `pg_stat_ssl`, so sessions PostgreSQL has not yet removed no longer fail
  the `pg-oracle` lane.
