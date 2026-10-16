### Rust MongoDB server: capped inserts in constant time, mongod's bucketing and resume-token errors

An insert into a full capped collection no longer reads the whole collection
to learn its size: the server keeps the collection's size and count from one
insert to the next, and counts again whenever anything else wrote the
collection, so a stale count can never evict the wrong documents. On a debug
build an insert into a full capped collection of 100,000 documents went from
84 ms to 0.4 ms, the same as at 10,000.

Time-series collections now place measurements the way mongod 8.2.11's bucket
catalog does: an insert batch is sorted by time first, a field that changes
type rolls the bucket over, and a measurement before the open bucket reopens
the earliest bucket whose window holds it. `$setWindowFields` emits its
partitions in partition-key order, and `$densify` with `bounds: "full"` and
`partitionByFields` fills every partition across the whole range, transcribed
from mongod's own stage. A resume token neither server can read now gets
mongod's own error, and the `Slow query` log line names the plan.

#### Fixed

- `secantus-storage`: capped-collection eviction keeps a running size and
  count instead of rescanning on every insert (RM-111; performance, pinned
  by `crates/secantus-storage/tests/capped_bounds.rs`, which holds eviction
  to a FIFO model through resizing updates, deletes, `collMod`, rename, drop,
  reopen and concurrent writers).
- Fixes RM-075 in part (tests/backlog/test_rm_057_084.py::test_undecodable_resume_token_errors):
  a resume token that is not this server's gets mongod's KeyString-decoder
  error (15, 50811, 50816, 40649, 50796, ... ; 37 shapes measured), from a
  transcription of mongod 8.2's decoder (`secantus-commands/src/resume_keystring.rs`).
- Fixes RM-078 (tests/backlog/test_rm_057_084.py::test_resume_token_not_found_message_names_the_event_found,
  tests/backlog/test_rm_057_084.py::test_transaction_event_uid_hashes_the_user):
  a 280 `resume token was not found` names the token of the event found in
  its place, as mongod does, and a transaction's change events carry
  `lsid.uid` = SHA-256 of `user@db`. The gap test's earlier premise (mongod
  cuts the token to 128 characters) was a misreading and is corrected.
- RM-088 in part (tests/backlog/test_rm_085_112.py::test_a_batch_is_placed_in_time_order,
  tests/backlog/test_rm_085_112.py::test_a_field_changing_type_rolls_the_bucket_over):
  time-series placement follows mongod's bucket catalog (batch sort, schema
  change, archive and query reopening).
- RM-089 in part: `createIndexes {_id: 1}` on a time-series collection builds
  the index, as mongod does (it answered "all indexes already exist").
- Fixes RM-097 (tests/backlog/test_rm_085_112.py::test_densify_full_partitions_fill_the_whole_range):
  `$densify` `bounds: "full"` across partitions; also a filler no longer
  carries a literal dotted key (`"a.b"`) or a `null` for a missing partition
  field, and a missing partition field is a partition of its own.
- Fixes RM-098 (tests/backlog/test_rm_085_112.py::test_set_window_fields_partition_order):
  `$setWindowFields` partitions come out in key order, under the collation.
- RM-023 in part (tests/backlog/test_rm_001_028.py::test_slow_query_line_names_the_plan):
  the `Slow query` line carries the command document, `planSummary`,
  `nreturned` and `reslen`.
