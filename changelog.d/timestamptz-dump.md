### A tz-aware datetime bound as a `timestamptz` parameter now round-trips

Binding a Python timezone-aware `datetime` as a `timestamptz` parameter on the
Rust PostgreSQL server (`secantusd-pg`) and comparing it to the same instant
written as a literal — `'<expr>'::timestamptz = %s`, the shape psycopg's own
`test_dump_datetimetz` asserts — returned `false`. The binary wire form of a
`timestamptz` parameter hands the server an absolute instant (i64 microseconds
since 2000-01-01 UTC), but the decoder rendered it to session-zone TEXT and
shipped THAT string as the value. The zone offset was then dropped the moment
anything re-coerced the string as a bare timestamp, so the parameter landed the
session offset away from the literal it was meant to equal and compared unequal.
Both `%b` (binary) and psycopg's default `%s` (which sends a datetime in binary)
were wrong across the corpus — year 0001 through 9999, sub-second fractions, and
seconds-carrying offsets; `%t` (forced text) was already correct.

The binary decoder now stores the instant directly, on the same carrier a
`::timestamptz` literal produces (a BSON date, or a sub-millisecond composite),
so the binary and text paths share one representation. A redundant
`timestamptz` → `timestamptz` cast (`$1::timestamptz` over a parameter already
declared `timestamptz`) is now a no-op instead of re-parsing the instant's
offset-less wall clock and applying the session zone a second time — the
`timestamp` → `timestamptz` cast still applies the zone, since its source type
is `timestamp`, not `timestamptz`. The psycopg gauge gains +33 passing tests
(3557 → 3590) with no regressions; `test_dump_datetimetz` goes from 30 failing
to fully green.

#### Fixed

- `secantus-pgserver` `decode_parameter`: a binary `timestamptz` parameter
  (oid 1184) is decoded to the instant carrier, not session-rendered text; the
  extreme i64 infinities are kept as `infinity` / `-infinity` text.
- `secantus-pgplan` `const_value`: a `timestamptz` → `timestamptz` cast over a
  stored instant is a no-op, so it no longer double-applies the session zone.
- `secantus-pgplan`: new `timestamptz_value_from_micros` builds the stored
  instant carrier, shared by the binary decoder.
