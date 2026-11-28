### An aggregate behind a dotted `$exists` explains and runs in SBE

The Rust MongoDB server now pushes a `$group` or `$count` behind a filter
such as `{"d.e": {"$exists": true}}` (or `false`) into the slot-based
engine, as mongod 8.2.11 does: each level above the last is traversed and
the last field's presence is tested.

#### Fixed

- `explain` of such an aggregate now prints mongod's `explainVersion: "2"`
  plan and its SBE execution stats (RM-071 in part,
  `tests/mongod_live/test_sbe_filtered_front_explain.py`).
