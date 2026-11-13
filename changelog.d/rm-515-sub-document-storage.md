### Rust MongoDB server: a sub-document a stage reads or writes into is sized as mongod stores it

A pipeline `$sort` and a `$setWindowFields` count each document's storage
the way mongod 8.2's classic engine keeps it (RM-408). That model now
follows a sub-document's own storage too: a field read inside it
(`$addFields: {q: "$e.y"}`) is cached there, a field written into it
(`$set: {"e.z": 1}`) is set on a copy of it, and a field set to it
(`q: "$e"`) shares it, so a later read inside it counts in both. The
`explain` counters `totalDataSizeSortedBytesEstimate` and
`maxTotalMemoryUsageBytes`, and the memory budget they feed, match mongod
8.2.11 for these pipelines.

#### Fixed

- A sub-document a stage read or wrote into was sized as the BSON it was
  read from: 202 bytes a document short for `$e.y`, 48 (a sort) and 121
  (a window) for `e.z`.
  Fixes RM-515 (tests/backlog/test_rm_found_rm408.py::test_a_sub_document_a_stage_touched_is_sized_by_its_storage);
  more shapes in `tests/mongod_live/test_sub_document_storage.py`. A
  document inside an array is not followed yet (RM-636, filed).
