### secantusd-mdb finishes an index build mongod left unfinished

A store mongod 8.2 stopped during an index build (the index `ready: false`
in its catalog, the build in `config.system.indexBuilds`) was refused by
name. `secantusd-mdb` now records the build under mongod's build UUID and
restarts it at the open, as mongod's restart does: the index is built, and
the build's `config.system.indexBuilds` row goes.

#### Fixed

- Opening a mongod store with an unfinished index build finishes the build
  instead of refusing the store.

Fixes RM-755 (tests/backlog/test_rm_storage_model.py::test_secantus_finishes_an_index_build_mongod_left)
