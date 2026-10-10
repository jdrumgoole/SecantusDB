### Rust MongoDB server: TTL indexes and the `bulkWrite` command, measured against mongod

Two more surfaces of the Rust MongoDB server were compared with `mongod`
8.2.11, scenario by scenario, and the differences fixed. TTL indexes had two
that lose or hide data. A partial TTL index expired every old document in the
collection, including the ones its filter leaves out. And an expiry was
written nowhere: no oplog entry, so a change stream never saw the delete and
recovery from the oplog could not replay it. Expiry is now an ordinary delete
with the index's filter, which fixes both and also expires a document whose
field is an array holding an old date.

The `bulkWrite` command (the 8.0 server command, not a driver's helper)
checked each operation as it reached it, so a malformed third operation left
the first two applied. It now checks the whole command before writing
anything, with `mongod`'s codes and messages. The probe also found four bugs
in plain `insert` and `update`, listed below.

#### Added

- `setParameter` / `getParameter` take `ttlMonitorSleepSecs` and
  `ttlMonitorEnabled`, and they act on the running monitor: a test can set
  the sleep to 1 second instead of waiting a minute.
- `serverStatus` reports `metrics.ttl` (`passes`, `deletedDocuments`), so a
  test can wait for a pass instead of sleeping.
- `update` statements take `c` (constants) and `upsertSupplied`; `bulkWrite`
  update operations take `constants` and `upsertSupplied`.
- `tools/probes/bulk_write_command.py` (141 scenarios) and
  `tools/probes/ttl_indexes.py` (50).

#### Fixed

- A partial TTL index no longer deletes documents outside its
  `partialFilterExpression`.
- A TTL expiry is written to the oplog and reaches change streams.
- A document expires when its TTL field is an array holding an expired date,
  directly or through a dotted path.
- Only a single-field index expires documents. `collMod` refuses to put a TTL
  on a compound index or on `_id`, refuses a negative or non-numeric
  `expireAfterSeconds`, and stores one past int32 as int32's largest;
  `createIndexes` refuses NaN. `expireAfterSeconds` is stored as an int32.
- The same index name and key with a different `expireAfterSeconds` is 85
  `IndexOptionsConflict`, as an equivalent index, where it was 86.
- A TTL sweep that failed on one collection reported success.
- `create` refuses `expireAfterSeconds` without `clusteredIndex` or
  `timeseries`.
- `insert` refuses an array or regex `_id` (53), and so does an upsert that
  would produce one. Both were stored.
- An upsert onto a taken `_id` reports the collection, index and key, where
  it answered a bare `E11000 duplicate key error`.
- A `$`-prefixed field in an inserted `_id` document is code 52, where it
  was 2.
- `bulkWrite`: nothing is written when any operation, `nsInfo` entry or
  option is malformed; an operation on a view fails with 166 where it was
  accepted; a failed
  operation's entry carries `errInfo` and, for an update, `nModified`; an
  unacknowledged write returns no per-operation results; a namespace index
  may be any numeric type.
- `createIndexes` refuses a `partialFilterExpression` using an operator a
  partial index cannot hold (`$ne`, `$nin`, `$not`, `$nor`, `$exists: false`,
  `$regex`, `$mod`, `$size`, `$elemMatch`, the `$bits*` operators, `$expr`,
  `$where`, `$text`, `$jsonSchema`).
