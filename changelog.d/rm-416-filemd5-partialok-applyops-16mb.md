### `filemd5` hashes a file in pieces, and `applyOps` inserts past 16 MB

A client can now hash a GridFS file a piece at a time on the Rust MongoDB
server, as it can on mongod: `filemd5` with `partialOk` answers the hash's
state with the digest, takes that state back with `startAt` to carry on,
and stops at a missing chunk instead of failing. The state is the same
416 bytes mongod 8.2 hands out, so one taken from either server resumes on
the other. `applyOps` now applies an insert of a document a little over
16 MB, as mongod's oplog application does, so an oplog replay carrying one
no longer fails on it.

#### Fixed

- `filemd5` with `partialOk` answered 115. It now answers `md5state`, the
  bytes of mongod 8.2's libtomcrypt `hash_state` (bit length, MD5 words,
  buffer fill and buffer, with the buffer's stale bytes and everything
  past the struct carried as mongod carries them), reads one back
  (`md5state` not BinData: 50847; not 416 bytes: 16247), reads `startAt`
  as mongod's `numberInt` (saturated, a double truncated, a decimal rounded
  half to even), and breaks at a skipped chunk. A state libtomcrypt
  refuses fails as mongod does, 12220700 "MD5 operation failed". The
  `notablescan` error names the `startAt` it queried from.
- An `applyOps` `i` entry for a document over 16 MB was 10334. Oplog
  application now inserts up to mongod's internal limit of 16 MiB plus
  16 KiB (`Storage::insert_from_oplog_application` in `secantus-storage`,
  which the PostgreSQL server also links but does not call); a `u` entry's
  upsert keeps the 16 MB limit, as on mongod.
- Fixes RM-416 in part
  (tests/backlog/test_rm_219_diag_query_settings.py::test_filemd5_partial_ok,
  tests/backlog/test_rm_219_diag_query_settings.py::test_apply_ops_inserts_past_16mb).
  RM-416 stays open for `dbCheck`'s `extraIndexKeysCheck`, which needs a
  KeyString encoder; filed RM-540 for `applyOps` `u` entries found along
  the way.
