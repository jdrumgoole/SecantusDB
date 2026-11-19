### The contested-write mongod test no longer pins where mongod cuts a delete's batches

`tests/mongod_live/test_contested_write_of_many.py` failed once in CI with both
servers reporting `n = 10` for a `deleteMany` but the oplog listing the ten
ids as one batch on `secantusd-mdb` and as 3 + 7 on `mongod`. All ten
documents were deleted on both; `mongod` commits a batched delete's batch when
its time-based yield fires, so where it cuts the run is timing.

#### Fixed

- The test now compares the ids every delete wrote, in the order the oplog
  names them, rather than the batch boundaries (and no longer picks up the
  multi-document insert entries that shared the oplog filter).
