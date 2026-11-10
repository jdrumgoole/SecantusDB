### A write of many on a clustered collection keeps what it wrote before running out of time

An `updateMany` or `deleteMany` on a clustered or time-series collection that
runs out of `maxTimeMS` part-way now leaves written what it had written, as
mongod does: mongod's update and delete stages write each document as their
plan returns it, whatever the plan. The Rust MongoDB server already did this
for plans it streams; a plan it cannot stream left the write to the store,
which read every candidate before it tested or wrote one, so a budget spent in
that read wrote nothing. The store now reads its candidates a page at a time --
a collection scan resumed where the last page ended, the documents an index
names fetched a page at a time -- and writes each page before it reads the next.

#### Fixed

- `secantus-storage`: the chunked updateMany and deleteMany read their own
  candidates a page of 1,000 at a time (`candidate_pages` /
  `next_candidate_page`), forward or `$natural: -1`, each page's matches
  written (a deleteMany's staged in batches of 10) before the next page is
  read. Both servers link this crate.
- Fixes part of RM-390 (tests/backlog/test_rm_found_2026_10_27.py::test_an_update_many_of_a_clustered_collection_out_of_time_keeps_its_updates);
  regression test in `crates/secantus-storage/tests/multiwrite_chunk.rs`
  (`a_write_of_many_reads_every_page_of_its_candidates_once`). RM-390 stays
  open for the reads that still run whole before the first write -- an
  index's RecordIds and the read that picks a contested plan
  (tests/backlog/test_rm_found_2026_11_10_rm390.py).
