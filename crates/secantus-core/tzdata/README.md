# mongod's time-zone database

`timezonedb.bin` and `timezonedb.idx` are the time-zone database mongod 8.2.11
compiles in: timelib's `timezonedb.h`, version "2024.2" (IANA tzdata 2024b
built with `backzone`, `zic -b fat`), from
`src/third_party/timelib/dist/zones/timezonedb.h` at mongodb/mongo `r8.2.11`.
They are generated, not edited: `tools/timezonedb_extract.py` copies them
byte for byte, and `--check` confirms the committed files still match.

- `timezonedb.bin`: the "fat" data array (mongod's build does not define
  `TIMELIB_SUPPORT_SLIM_FILE`). Each zone is timelib's 20-byte `PHP2`
  preamble, then TZif v2 data with its POSIX footer.
- `timezonedb.idx`: the version, then one `name<TAB>offset` line per zone.

`src/tzdb.rs` reads them as timelib does. The Rust MongoDB server answers
every time-zone question from this data, so its zones match mongod's in
history, far future and names (RM-249; Joe chose mongod's data over a tz
crate on 2026-10-22).

**Refresh** when the targeted mongod moves to a new timelib database:
`python tools/timezonedb_extract.py --ref r<version>`, then run
`tools/probes/time_zones.py` against that mongod.

The tz data is in the public domain. timelib is MIT-licensed.
