### A DISTINCT_SCAN counts the key past its bounds

A `distinct` (or a `$group`) answered from an index's DISTINCT_SCAN now
reports the keys it examined as mongod does. mongod's DISTINCT_SCAN has no
end key: its last seek lands on the key past the bounds and examines it
before it finds it is done, so `distinct("x", {x: {$lt: 4}})` over keys 0
to 11 examines five keys, not four. The Rust MongoDB server's profile
entries, `Slow query` lines and their `execStats` counted four; they now
count the key mongod's scan lands on, and none when the bounds run to the
end of the index.

#### Fixed

- `secantus-commands` (`explain/stream.rs`): a streamed DISTINCT_SCAN
  whose bounds end before the index does counts the key past them in
  `keysExamined`, as `DistinctScan::doWork` does.
- Fixes RM-377 (tests/backlog/test_rm_found_2026_11_07.py::test_a_distinct_scan_examines_the_key_past_its_bounds);
  regression tests in `tests/mongod_live/test_distinct_scan_keys.py`
  (forward, descending, runs of equal keys, bounds to the index's end,
  `explain`, and a `$group`).
