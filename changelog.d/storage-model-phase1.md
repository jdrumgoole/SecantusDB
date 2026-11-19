### Stores are written by WiredTiger as mongod builds it

SecantusDB's two Rust servers now write their stores with WiredTiger built
the way mongod builds it, compressing with mongod's own snappy, and keep the
WiredTiger log in `journal/` beside mongod's `storage.bson`, as a mongod data
directory does. This is the first step of moving the on-disk layout to
mongod's, so that a mongod can open a SecantusDB store and SecantusDB can open
mongod's.

**This changes the on-disk format.** A store written by an earlier release is
refused when it is opened, with a message saying what to do: dump it with the
release that wrote it (`mongodump`, or `pg_dump` for the PostgreSQL server)
and restore it into a new store. There is no in-place migration.

#### Changed

- WiredTiger is built with `WT_STANDALONE_BUILD=OFF`, as mongod's is, in all
  three builds (the wheel's CMake, `secantus-wiredtiger-sys`,
  `./inv rust-wt-build`). With it on, WiredTiger stamps every file btree
  version 2.1, which mongod refuses.
- New tables use `block_compressor=snappy` (mongod's), on every platform,
  Windows included.
- The WiredTiger log lives in `journal/` and is snappy-compressed.
- Every store carries mongod's `storage.bson` and a `secantus.format` file
  with its layout version; backup archives include both, and log files under
  `journal/`.

#### Added

- `vendor/snappy` (1.1.10) and `vendor/zstandard` (1.5.5): mongo r8.2.11's
  own copies, built by the new `secantus-compressors` crate (and by the
  wheel's CMake) into WiredTiger's builtin snappy and zstd extensions.
  Release archives and the PyPI wheel carry their licences.
- `secantus_storage::store_format`: the layout check that runs before
  WiredTiger opens a store, and refuses a store from an earlier release, or
  (for now, RM-723) one mongod wrote, by name.
