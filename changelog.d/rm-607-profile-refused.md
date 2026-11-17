### Rust MongoDB server: a refused command is profiled as mongod profiles it

A command mongod refuses before it locks its database is no longer
profiled, and a `distinct` refused for its fields now is.

#### Fixed

- `distinct` refused for an unknown or missing field writes its profile
  entry; a `findAndModify` naming an undefined `$$variable`, a
  `createIndexes` with an unreadable spec, a capped `create` without a
  size, an `insert` into a `system.` namespace and an `explain` of an
  `insert` write none (measured against mongod 8.2.11).
  Fixes RM-607 (tests/backlog/test_rm_found_rm386.py::test_a_command_refused_as_it_runs_is_profiled_as_mongod_profiles_it)
