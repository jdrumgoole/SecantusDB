### The spill engine compresses its tables with zstd at level -7, as mongod 8.2's

A `$setWindowFields` (or any stage) that spills to `<dbpath>/_tmp/spilldb`
now writes zstd-compressed pages at level -7, and opens that engine with
the configuration mongod 8.2.11 logs (`builtin_extension_config=(zstd=(compression_level=-7))`).

#### Changed

- Spill tables are `block_compressor=zstd`. Part of RM-611; its size on disk
  still differs, because WiredTiger 7.0 splits forced-evicted pages in memory
  where mongod's 12.0 writes them (tests/backlog/test_rm_found_2026_11_13_rm325.py::test_a_window_spill_the_engine_writes_out_is_mongods_size).
