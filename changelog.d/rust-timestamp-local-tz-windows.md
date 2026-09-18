### The Rust engine ignored `TZ` on Windows, and no CI lane could see it

`$toLower` / `$toUpper` of a `Timestamp` is the one conversion mongod renders in
the server process's *local* time. The Rust engine resolved that zone through
`chrono::Local`, which on Windows reads `GetDynamicTimeZoneInformation` and
ignores the `TZ` environment variable entirely — so a server started with
`TZ=UTC` on a `Europe/London` host rendered summer instants an hour late, while
mongod and the pure-Python evaluator both answered UTC. The two SecantusDB
servers disagreed with each other and one of them disagreed with mongod.

Three independent things hid it. GitHub's Windows runners are UTC, where the
host zone and `TZ=UTC` coincide; the Windows test lane has no `_secantus_core`,
so the test that would have caught it skipped; and the `storage-engine` job runs
only three smoke files. It took building and running on a non-UTC Windows box in
July to surface — London *is* UTC in winter, so three of the four instants in
the existing corpus agree even with the bug present.

The fix routes the Windows render through the MSVC CRT (`_tzset` +
`_localtime64_s`), which is the same function mongod and CPython's
`time.localtime` reach. That is exact by construction rather than an emulation
of the CRT's `tzn[+|-]hh[:mm[:ss]][dzn]` grammar — a grammar that is *not* the
IANA one, and observably so: measured against mongod 8.2.11 on Windows 11,
`TZ=America/New_York` parses as a zone name with a zero offset plus a daylight
rule, so mongod answers UTC+1 in July, not New York's UTC-4. Unix keeps
`chrono::Local`, which already matched.

#### Fixed

- `crates/secantus-core`: `$toLower` / `$toUpper` of a `Timestamp` now honours
  `TZ` on Windows, matching mongod and the Python engine. New
  `expressions::render_local_asctime` splits the render per platform; the
  Windows half calls the CRT via a `cfg(windows)` `libc` dependency.

#### Added

- `tests/test_mongod_differential.py`: `test_timestamp_local_render_matches_mongod`
  spawns a mongod per zone and compares both engines against it, with no
  hardcoded expectations — so unlike the sibling unit test, which pins
  Unix-measured values and must skip zone-shifted cases on Windows, it needs no
  platform skip and asserts whatever the local mongod actually does. The engine
  is a parameter, so the Rust half is a visible skip where that extension is
  absent rather than a silent one.
- `tests/test_mongod_differential.py`: `_start_mongod` factors the spawn,
  readiness poll and port-race guard out of the module fixture, so cases that
  need a differently-configured server reuse them instead of re-deriving them.
