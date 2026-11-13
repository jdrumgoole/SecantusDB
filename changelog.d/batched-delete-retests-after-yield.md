### A batched deleteMany tests a document again after a yield

mongod deletes a deleteMany's documents in batches of ten: it stages each
document in one step of its plan and deletes the batch in a later one. Its
executor may yield between those steps, which drops the snapshot, and it
then reads each document staged before the yield again and tests the
filter again before deleting it. A deleteMany of one document whose filter
takes a second to test takes two seconds on mongod 8.2.11. With documents
that test in a fifth of a millisecond it is about 6% slower than a `find`
of them, and with `batchUserMultiDeletes` off it tests each document once.
The Rust MongoDB server tested each document once, so a deleteMany
finished inside a `maxTimeMS` that mongod's runs out of (code 50).

The batched delete now yields where mongod's executor does: before each
step that stages a document, and before the step that finds the end of the
read and deletes the last batch. Each staged document records the
statement's yield count, and a document staged before a later yield is
tested again when its batch is deleted. A document that fills its batch is
deleted in the same step as it is staged, and is tested once. A collection
that records pre-images is not batched by mongod, and still tests once.

#### Fixed

- A batched `delete` of many documents tests a document's filter a second
  time when the statement yielded after it staged the document, as
  mongod 8.2.11 does (`ensureStillMatches`). This applies to both the plan's
  streamed read and the store's own candidates. The change is in
  `secantus-storage`, which the PostgreSQL server also links. That
  server's unbounded deletes reach the same loop, but it sets no yield
  hook and arms no deadline, so each work boundary there only reads the
  clock and no document is tested twice.
  Fixes RM-501
  (`tests/backlog/test_rm_found_2026_11_10_rm391.py::test_a_batched_delete_many_tests_each_document_twice`);
  unit test `deadline::tests::a_work_boundary_yields_on_its_first_call_past_the_period`.
- Filed RM-609: a deleteMany's `system.profile` entry says `numYield: 0`,
  where mongod counts its yields (5 for three half-second documents)
  (`tests/backlog/test_rm_found_rm501.py::test_a_delete_many_profile_entry_counts_its_yields`).
