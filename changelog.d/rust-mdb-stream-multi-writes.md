### A multi-document write keeps what it wrote before an interrupt

An `updateMany` or `deleteMany` that is killed or runs out of `maxTimeMS`
part-way through its read now leaves written what it had written, as mongod
does. mongod's update and delete stages pull each document from the plan and
write it before reading the next -- a deleteMany staging ten at a time -- so
an interrupted write has done part of its work. The Rust MongoDB server used
to read and test every candidate first, and an interrupt during that read
left nothing written. The write now draws its documents from the plan's
stream a pull at a time, and the filter each document matched in that read
is not tested again.

#### Fixed

- `secantus-commands` (`explain/order.rs`, `explain/stream.rs`, `crud.rs`):
  a write of many runs its plan's read as it writes (`Pin::Read`), keeping
  the documents a read that fails part-way matched before the failure.
- `secantus-storage`: `with_candidate_source` lets the chunked update and
  delete draw a pull at a time; a deleteMany deletes each staged batch of 10
  (or 2 MiB) as it is read, including over its own candidates, and an
  updateMany writes a document an index scan meets twice once. Both servers
  link this crate.
- Fixes RM-323 (tests/backlog/test_rm_found_2026_10_27.py::test_a_killed_delete_many_keeps_the_batches_it_deleted,
  tests/backlog/test_rm_found_2026_10_27.py::test_an_update_many_out_of_time_keeps_the_documents_it_updated);
  regression tests in `crates/secantus-storage/tests/candidate_source.rs`.
  Files RM-390 (a plan the write cannot stream still reads whole first).
