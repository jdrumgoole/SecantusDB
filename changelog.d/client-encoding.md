### The Rust PostgreSQL server honours `client_encoding`

`secantusd-pg` stores and evaluates everything in UTF-8, but until now it
ignored `SET client_encoding` and hard-coded `client_encoding = UTF8` in its
startup reply. A client that asked for `LATIN9` still received UTF-8 bytes,
and — worse — nothing told the client its request had been dropped, so the two
sides silently disagreed about the wire encoding. The one thing the server was
careful *not* to do was report a `client_encoding` it did not actually honour:
a `ParameterStatus` for an unhonoured GUC makes libpq switch its own codec and
mis-decode every value, which is how a comparable `DateStyle` change once
regressed 160 tests.

The server now transcodes result text to the client encoding and decodes
incoming text parameters from it, in both the text and binary wire formats,
for `LATIN1` and `LATIN9`; `UTF8` and `SQL_ASCII` (and every other accepted
name) keep the internal UTF-8 bytes exactly as before, so a UTF-8 client is
byte-for-byte unaffected. A character with no representation in the target
encoding raises PostgreSQL's `22P05` untranslatable-character error, with the
same message text a real server emits. `SET client_encoding` (and
`set_config('client_encoding', …)`) now validates the name against
PostgreSQL's encoding table — an unknown name is `22023`, `MULE_INTERNAL` is
`0A000` — stores the canonical spelling, and reports it via `ParameterStatus`
only once output genuinely respects it.

The encoding is honoured on the way in as well. The query text itself — a
literal `select 'café €'` or an alias `as "prix €"` typed by a LATIN9 client —
used to be decoded as UTF-8 and reach the planner as U+FFFD mojibake, because
the wire library discarded the original bytes before the server saw them. The
vendored `pgwire` now keeps the raw `Query` / `Parse` bytes and lets the
server decode them in the session encoding, and RowDescription column names
travel back in that encoding too. A `client_encoding` in the startup packet
(libpq's `PGCLIENTENCODING`, psycopg's `client_encoding=` option) is applied
before the first query and re-reported under its canonical name; an unknown
one fails the connection with PostgreSQL's FATAL `22023`.

#### Added

- `crates/secantus-pgserver/src/encoding.rs`: client-encoding name
  canonicalisation (PostgreSQL's `clean_encoding_name` rules plus the
  `ISO-8859-N` / `UNICODE` aliases) and LATIN1 / LATIN9 byte transcoding, with
  unit tests.

#### Fixed

- `crates/secantus-pgserver/src/lib.rs`: track `client_encoding` per session
  from `SET` / `set_config` / (validated, canonicalised); transcode text output
  in `encode_field_value` and COPY-OUT text, decode text parameters in
  `decode_parameter`, and report `client_encoding` via `ParameterStatus` now
  that output honours it.
- `crates/secantus-pgserver/src/lib.rs`: decode the simple-query and `Parse`
  SQL text in the session `client_encoding` (`decode_query_text`), send
  RowDescription column names in it (`transcoded_name`), and apply a startup
  `client_encoding` parameter in `post_startup`. `SHOW <guc>` no longer holds
  the settings lock while building its result column (a deadlock once that
  column name went through the encoding lookup).
- `crates/vendor/pgwire`: `Query` / `Parse` carry `query_raw` (the undecoded
  C-string bytes) and both query handlers gain a `decode_query_text` hook;
  `FieldInfo` / `FieldDescription` gain `name_raw` so a handler can send a
  column name in a non-UTF-8 client encoding. The `cursor` example is updated
  for the earlier `parameter_oids` patch.
