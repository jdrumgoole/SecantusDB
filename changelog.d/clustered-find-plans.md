### A find on a clustered collection is planned as a scan of the table

`explain` of a read on a collection made with `clusteredIndex` named an
`IXSCAN` on `_id_` over a `FETCH`, an index the collection does not have. The
table is keyed by `_id`, and mongod 8.2.11 reads it as such: an `_id` equality
is an `EXPRESS_CLUSTERED_IXSCAN`, and any other predicate on `_id` a
`CLUSTERED_IXSCAN` between two `_id`s.

#### Fixed

- Fixes RM-739 (`tests/backlog/test_rm_clustered_explain.py::test_an_id_point_read_is_an_express_clustered_scan`,
  `::test_an_id_range_is_a_bounded_clustered_scan`). The planner takes the
  clustered key out of its catalog and offers the bounded scan as one more
  candidate (mongod's `collscanIsBounded`), so it is tried against the indexes
  other predicates use: `{_id: {$gt: 150}, x: 1}` still reads `x_1` where that
  is cheaper, `{_id: 100, x: 2}` reads the table. `minRecord` and `maxRecord`
  are the bounds as mongod prints them (a number's lower end is `NaN`, a
  string's `""`, an open upper end the type bracket's), an `$in` is the range
  of its first and last value with the `$in` kept as the filter, and what the
  bounds decide leaves the filter, an `$and` of the rest. A sort on `_id` is
  the scan's direction, with no `SORT`. A hint of the key (by the name it was
  created with, or `{_id: 1}`) and `{$natural: ±1}` are the same scan, and
  the bounded scan reads no table under `notablescan`.
- The scan's counters are mongod's: the seek to its start examines nothing,
  the first record past its end counts as examined, and `works` is one more
  where it ran off the table. An express `_id` lookup (and `EXPRESS_UPDATE` /
  `EXPRESS_DELETE`) examines no key and names no index.
- Regression tests: `tests/mongod_live/test_clustered_scan_plans.py` compares
  the plan, the plans it beat and the counters of 62 finds with mongod, and
  the order of the documents they return.

#### Found

- What this left for other items, each with a failing test: `count`,
  `distinct`, `update` and `delete` over an `_id` range (RM-816), an `$or` of
  `_id` ranges (RM-817), `min` / `max` over the key (RM-818), the plan cache
  and plan trial of a read of a clustered collection (RM-819), a `$group`
  over a range in the SBE (RM-820) and a hint by the key's name on a `find`
  (RM-821).
