### Rust MongoDB server: a replica-set member builds `config.system.sessions` at its first refresh, as mongod does

A fresh mongod 8.2.11 creates `config.system.sessions` and its
`lsidTTLIndex` as it starts only when it is a standalone. A replica-set
member leaves them to its logical session cache's first refresh, a
`logicalSessionRefreshMillis` after it steps up (299 s under the default
300,000; measured 2026-10-28). This server created them at startup in both
topologies, so for the first refresh interval a set member listed a
collection mongod does not have. The comparison test that exposed this
passed or failed depending on which test had last refreshed the mongod it
shared.

Three tests that failed on a developer's Mac or under load now measure what
they mean to:

- the `$setWindowFields` budget comparison translates a macOS mongod's
  sizes to the Linux build's, the exemplar, for the two shapes where they
  differ (an unbounded `$sum` is 184 bytes there and 176 on Linux);
- the RM-230 and RM-313 regression tests wait for the slow update to be
  running before they retry or kill it, re-time the slow update they
  calibrate, and report a missed measurement as one rather than as a
  regression.

#### Fixed

- `secantusd-rs` started as a replica-set member (the default) no longer
  creates `config.system.sessions` at startup; a standalone still does
  (`tests/mongod_live/test_logical_sessions.py::test_a_set_member_builds_the_sessions_collection_at_its_first_refresh`).

#### Filed

- RM-366: a set member does not create the five other `config`
  collections mongod creates as it steps up.
