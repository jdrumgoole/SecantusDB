### A standalone mongod store killed in an index build opens

`secantusd-mdb` refused the store of a standalone mongod 8.2 that was killed
while an index was being built ("is unfinished and names no build UUID"), which
made the RM-808 test fail about three runs in eight. mongod does not restart
such a build: its startup drops the index ("Dropping unfinished index") and
serves the collection without it, and so does `secantusd-mdb` now. A build that
names a build UUID (a replica set's) is still restarted, as before.

#### Fixed

- A catalog entry with `ready: false` and no `buildUUID` is dropped at open, as
  mongod does. Fixes RM-811
  (`tests/backlog/test_rm_storage_model.py::test_secantus_drops_an_index_build_a_standalone_mongod_left_unfinished`).
