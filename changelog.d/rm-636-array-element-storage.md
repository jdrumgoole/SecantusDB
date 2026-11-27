### A document inside an array is sized as mongod stores it

The Rust MongoDB server's storage model now follows an array's elements:
a stage that reads a field through an array (`$t.y`) caches it in each
document element's own storage, and one that writes through it (`t.w`)
copies each document element with the field set and builds a document in
each scalar's place, as mongod 8.2's classic engine does. The `$sort` and
window behind count what mongod counts.

#### Fixed

- `totalDataSizeSortedBytesEstimate` and `maxTotalMemoryUsageBytes` after
  an `$addFields` / `$set` through an array match mongod 8.2.11.

Fixes RM-636 (tests/backlog/test_rm_found_rm515.py::test_an_arrays_documents_a_stage_touched_are_sized_by_their_storage)
