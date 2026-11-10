### A deleteMany over pre-images deletes one document at a time, as mongod does

mongod batches a multi-document delete, writing ten documents at a time as
one `applyOps` oplog entry -- except on a collection with
`changeStreamPreAndPostImages` enabled, which it deletes one document at a
time in its plan's order: each document gets its own `d` entry, its own time,
and its own pre-image at `applyOpsIndex` 0. The Rust MongoDB server batched
both kinds, so on a pre-image collection its oplog, its change stream event
times and its pre-images' `_id` all differed from mongod's. It now deletes
such a collection one document at a time too. A delete whose filter fails
part-way keeps every document deleted before the failure, as on mongod
8.2.11, where a batched delete of a plain collection keeps none of the batch
it was staging.

#### Fixed

- `secantus-storage`: the chunked `deleteMany` stages one document per
  batch when the collection records pre-images, so no batch group or shared
  `applyOps` is formed. The PostgreSQL server links this crate; it does not
  set the option.
- Fixes RM-389 (tests/backlog/test_rm_found_2026_11_07c.py::test_a_delete_many_over_pre_images_is_not_batched)
- Regression tests: `crates/secantus-storage/tests/pre_images.rs`
  (`a_delete_many_s_pre_images_are_one_per_entry`,
  `a_failing_delete_many_over_pre_images_keeps_what_it_deleted`).
