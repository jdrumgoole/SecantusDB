### The initiation logs the creation of `config.system.preimages`

`secantusd-mdb` created the change-stream pre-images collection without an
oplog entry; mongod 8.2.11's initiation logs it as `{op: "c", ns:
"config.$cmd", o: {create: "system.preimages", clusteredIndex: ...}}`, so a
mongod replaying the oplog from genesis now finds it.

#### Fixed

- The creation of `config.system.preimages` is logged; its documents stay
  unreplicated.

Fixes RM-748 (tests/backlog/test_rm_storage_model.py::test_the_initiation_logs_the_pre_images_collection)
