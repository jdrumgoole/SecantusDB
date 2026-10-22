### Rust MongoDB server: time zones come from mongod's own zone database

mongod does not read the system's time-zone files. It compiles in timelib's
timezonedb, which for 8.2.11 is version "2024.2": IANA tzdata 2024b built
with the `backzone` history, 597 zone names, and POSIX rules that carry
daylight saving past a zone's last stored change. The Rust MongoDB server
answered from chrono-tz instead, which carries tzdata 2025b without
`backzone` and computes daylight saving only up to 2099. Across 599 names
and 58 instants from 1850 to 9000, 2,005 `%z` answers differed from mongod.

The server now reads mongod's own data: `crates/secantus-core/tzdata/` is
that database, copied byte for byte from mongod's source by
`tools/timezonedb_extract.py`. A port of timelib's reader in
`secantus-core/src/tzdb.rs` reads it. Every time-zone answer comes from it:
`$dateToString`, the date-part operators, `$dateToParts`, `$dateFromParts`,
`$dateFromString`, `$dateAdd`, `$dateTrunc` and `$dateDiff`.
`tools/probes/time_zones.py` asks both servers 75,226 questions over 601
names, in both directions, and finds no difference. The Rust PostgreSQL
server keeps its own zone handling; PostgreSQL is its reference.

#### Fixed

- Daylight saving continues past 2099. New York in July 2100 or 9000 is
  -0400, as on mongod.
- Zone history follows `backzone`, as mongod's does. For example, Amsterdam
  in 1900 is +0019 and Reykjavik in 1930 is -0100.
- Today's rules are 2024b's, as mongod's are: Asuncion stays on its old
  rules in 2025.
- Zone names are mongod's, matched exactly: `Factory` is a zone,
  `America/Coyhaique` and `america/new_york` are 40485.
- `%z` takes its sign from the offset in seconds, so Accra's local mean time
  of -0:00:52 renders as `-0000`.
- A local time at a daylight-saving change goes through timelib's own
  `do_adjust_timezone` for every operator. The daylight-saving flag comes
  from the zone at the 1970 epoch, or from the start date in `$dateAdd`.

Fixes RM-249
(tests/backlog/test_rm_found_2026_10_21b.py::test_time_zone_rules_far_from_now).
