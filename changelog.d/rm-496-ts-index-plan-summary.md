### Rust MongoDB server: a time-series read names its index as the buckets keep it

A read of a time-series collection through one of its indexes now shows
that index in its `planSummary` the way mongod 8.2.11 does: as the index
over the collection's buckets. This applies to the `Slow query` log line
and the profile entry.

#### Fixed

- A `find`, `aggregate`, `count` or `distinct` of a time-series collection
  read through an index used to show the index as `listIndexes` lists it,
  for example `IXSCAN { m: 1, t: 1 }`. It now shows the index as mongod
  names it over the buckets. The meta field is shown as `meta`. A
  measurement field is shown as its pair of bounds, with `control.max`
  first for a descending key:
  - `IXSCAN { meta: 1, control.min.t: 1, control.max.t: 1 }` for the
    default meta-and-time index;
  - `IXSCAN { meta.a: 1, control.max.t: -1, control.min.t: -1 }` for an
    index on `{m.a: 1, t: -1}`;
  - `IXSCAN { control.min.v: 1, control.max.v: 1 }` for an index on
    `{v: 1}`.
- Fixes RM-496 (tests/backlog/test_rm_found_2026_11_10_rm363.py::test_a_read_of_the_meta_index_names_the_index_over_the_buckets),
  with every other kind of index and read pinned by
  `test_every_read_of_a_time_series_index_names_it_over_the_buckets`.
- Filed RM-600: when a time-series read is bounded on its time field and no
  index serves the bound, mongod reports a `CLUSTERED_IXSCAN` of the
  buckets, but this server reports `COLLSCAN`.
