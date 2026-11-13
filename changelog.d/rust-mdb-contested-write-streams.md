### A contested write of many is tried as mongod tries it, and writes as it reads

An `updateMany` or `deleteMany` whose query two indexes can answer now chooses
its plan the way mongod does -- its candidates tried, through the plan cache,
over only what their scans reach -- and then writes each document the winning
plan returns as it returns it. The Rust MongoDB server used to read every
match of such a query before it wrote the first, to work out which plan
mongod would keep, so a write that ran out of `maxTimeMS` in that read wrote
nothing; mongod has written some by then.

#### Fixed

- A contested updateMany or deleteMany out of `maxTimeMS` part-way keeps what
  it wrote (mongod 8.2.11: 1,536 updated and 10,030 deleted of 100,000 with
  `maxTimeMS: 30`). The trial runs over what each candidate's scan reaches
  (`explain::order::tried_write`), and the winner streams into the write as
  an uncontested plan does. Its plan cache entry, its `planSummary`, the keys
  and documents it reports examined, and a deleteMany's oplog batches are
  mongod's (`tests/mongod_live/test_contested_write_of_many.py`). An index
  intersection, a rooted `$or`, a sort, a geo predicate or an `$expr` still
  reads as before.
- Fixes part of RM-390 (tests/backlog/test_rm_found_2026_11_10_rm390.py::test_a_contested_write_of_many_out_of_time_keeps_what_it_wrote);
  RM-390 stays open for an index of a clustered collection, whose RecordIds
  are still read whole before the first write
  (tests/backlog/test_rm_found_2026_11_10_rm390.py::test_a_write_of_many_out_of_time_keeps_what_it_wrote_whatever_its_plan).
- Filed RM-601: a profile entry's `keysExamined` and `docsExamined` are int64,
  where mongod writes int32.
