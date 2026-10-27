### Rust MongoDB server: a drop that aborts an index build logs after the build has finished

A `drop` or `dropDatabase` that aborts a running index build now waits until
the build's last write, the delete of its `config.system.indexBuilds` row,
is in the oplog before it logs its own `drop`, as mongod 8.2.11 does (30 of
30 runs). Before, the build was unregistered first, so the `drop` could log
ahead of the row's delete.

#### Fixed

- The oplog order of an index build's abort and the `drop` that caused it
  (`tests/mongod_live/test_index_builds.py::test_what_ends_a_build[drop]`,
  which failed on main's CI run 40457857774).
