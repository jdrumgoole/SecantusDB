### A time-series find is the aggregation mongod makes of it

mongod answers a `find` of a time-series collection the way it answers a
`find` of a view. It converts the find to an aggregation over the
collection's buckets and reads that through its cursor stage. The Rust
MongoDB server read the whole collection when the `find` ran. A cursor
opened before a delete therefore returned every measurement it had matched,
and the slow-query line and the profile entry counted nothing examined.

The `find` is now that aggregation here as well. It buffers its buckets as a
time-series `aggregate` does, and counts what it examined as mongod does:
five buckets for the first batch over five buckets, none for the `getMore`
after it. The conversion also refuses the `find` options an aggregation
does not take, for a view as well as a time-series collection, in mongod's
order.

#### Fixed

- `secantus-commands` (`find.rs`): a `find` of a time-series collection
  becomes the aggregate a `find` of a view becomes (after its hint is
  checked over the buckets), so it streams through the bucket cursor where a
  time-series `aggregate` does. `find` no longer counts the index use a
  second time: the aggregate counts it. The conversion carries
  `allowDiskUse`.
- `aggregation_refusal`: a `find` of a view or a time-series collection
  refuses `min`, `max`, `returnKey`, `showRecordId`, `tailable`,
  `noCursorTimeout`, `allowPartialResults`, `singleBatch` (but with
  `limit: 1`) and `readOnce`, in that order (168, as mongod 8.2.11 does).
  Before this it refused only `tailable`, `min` and `max`, and checked
  `tailable` first.
- Fixes RM-455
  (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_find_goes_bucket_by_bucket)
  and RM-497
  (tests/backlog/test_rm_found_2026_11_10_rm363.py::test_a_find_converted_to_an_aggregation_refuses_what_an_aggregation_does_not_take).
- Fixes RM-363 in part
  (tests/backlog/test_rm_355_364.py::test_a_read_of_a_time_series_collection_counts_what_it_examined,
  tests/backlog/test_rm_355_364.py::test_a_time_series_find_counts_the_buckets_its_first_batch_examined).
  Still open: contested `aggregate` / `count` / `distinct` / write reads,
  time-series reads made whole, and geo reads.
- Filed RM-496 (the meta index's `planSummary` on a time-series read) and
  RM-498 (a tailable `find` of a collection that is not capped prints no
  query), with gap tests in tests/backlog/test_rm_found_2026_11_10_rm363.py.
