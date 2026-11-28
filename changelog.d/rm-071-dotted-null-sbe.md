### An aggregate behind a null test on a dotted path explains and runs in SBE

The Rust MongoDB server now pushes a `$group` or `$count` behind a filter
such as `{"d.e": null}` (or its `$ne`) into the slot-based engine, as
mongod 8.2.11 does: each level is traversed only when it is an object or
an array, and a missing level matches unless its parent is an array.

#### Fixed

- `explain` of such an aggregate now prints mongod's `explainVersion: "2"`
  plan and its SBE execution stats (RM-071 in part,
  `tests/mongod_live/test_sbe_filtered_front_explain.py`).
