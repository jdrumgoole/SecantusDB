### Rust MongoDB server: `dbCheck` runs `extraIndexKeysCheck`

`dbCheck` with `validateMode: "extraIndexKeysCheck"` walks the named index
as mongod 8.2's primary does, instead of refusing it (115). Each `batch`
oplog entry carries the MD5 of its keys in mongod's KeyString V1 encoding,
and batches end only where the key changes. `batchStart`, `batchEnd` and
`lastKeyChecked` are what mongod writes. This covers every BSON type except
Decimal128, ascending, descending, compound, multikey, sparse, partial and
collated indexes, and `start` / `end` / `maxCount`. A missing index gets
mongod's `dbCheckBatch` warning in `local.system.healthlog`.

#### Fixed

- `dbCheck` `extraIndexKeysCheck` answers as mongod does. Decimal128 keys
  and 2d, 2dsphere and wildcard indexes are still refused by name (RM-688).

Fixes RM-416 (tests/backlog/test_rm_219_diag_query_settings.py::test_db_check_extra_index_keys,
tests/mongod_live/test_db_check_extra_index_keys.py::test_extra_index_keys_check_as_mongod)
