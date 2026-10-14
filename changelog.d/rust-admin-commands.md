### The Rust MongoDB server has five more administrative commands

`dbHash`, `logRotate`, `connPoolStats`, `setDefaultRWConcern` and
`setFeatureCompatibilityVersion` were `CommandNotFound`, which is what a
tool checking two servers' data, rotating logs, reading pool statistics or
setting the cluster's defaults got. Each is now what mongod 8.2.11 answers,
measured as a one-member replica set by `tools/probes/admin_commands.py`:
279 of 281 replies agree, and the two that do not are the system databases'
own collections (`admin.system.keys`, `config`'s internal collections),
which this server does not have.

`dbHash`'s digests are mongod's for the same documents: a collection is the
MD5 of its documents' BSON in `_id` order under its default collation, a
capped collection in insertion order, the database the MD5 of the
collections' digests in name order. Views are left out and `system.views`
is hashed. `system.profile` and everything in `local` are left out too,
because mongod does not replicate them. `setDefaultRWConcern` stores the defaults in
`config.settings` as mongod does, so they survive a restart and
`getDefaultRWConcern` reports where they came from and when they were set;
a write that sends no write concern of its own is held to the stored
default, so a default of `w: 3` answers every write with mongod's
`customDefault` error and `setDefaultRWConcern` itself fails while still
storing its value. `setFeatureCompatibilityVersion` keeps the version in
`admin.system.version`, where `getParameter` reads it back, with mongod's
rules for `confirm`, `dryRun`, the three versions a binary of 8.2 accepts
and the one (8.1) it will not move to. What it does not do is gate
features by the version. `logRotate` answers as a mongod logging to its
stream does; `connPoolStats` reports no pools, which is what this server
holds. Each command needs the privilege mongod needs.

#### Added

- `crates/secantus-commands`: `dbhash.rs`, `rwconcern.rs` (which also
  holds `getDefaultRWConcern`) and `cluster_admin.rs`; the five commands
  in `authz.rs`; `dispatch` applies the stored default write concern.
- `tools/probes/admin_commands.py` is the measure, and
  `crates/secantus-storage-adapter/tests/admin_commands_wt.rs` pins its
  answers.

#### Fixed

- `getDefaultRWConcern` echoed `inMemory: false`; mongod echoes it only
  when true. `logRotate`, `connPoolStats`, `dbHash` and
  `getDefaultRWConcern` refuse a `writeConcern` as mongod does.
