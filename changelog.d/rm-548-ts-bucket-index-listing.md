### Rust MongoDB server: a time-series collection's indexes list on its buckets as mongod keeps them

`listIndexes` of `system.buckets.<name>` now shows each index of a
time-series collection as mongod 8.2.11 keeps it on the buckets.

#### Fixed

- A TTL index on the time field is keyed `{control.min.<time>: 1}` alone,
  not the `control.min` / `control.max` pair.
- A partial index's filter is listed translated to the buckets: the meta
  field's predicates on `meta` with an implicit equality spelled `$eq`, a
  measurement's comparisons as `control.min` / `control.max` bounds, the
  time field's without a bucket `_id` bound.
- A partial index carries the spec as asked in `originalSpec`, which the
  collection's own `listIndexes` does not show.

Fixes RM-548 (tests/backlog/test_rm_found_2026_11_11_rm088.py::test_bucket_indexes_list_a_ttl_index_as_mongod_keeps_it)
