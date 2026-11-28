### An aggregate behind an `$exists` runs in SBE

The Rust MongoDB server now runs a pushed-down `$group` or `$count` behind an
`$exists` on a one-field path as a slot-based plan, as mongod 8.2.11 does:
`exists(s)` of the field's slot, and `!(exists(s))` for `$exists: false`. The
`explain` (`explainVersion: "2"`, `queryPlan`, `slotBasedPlan`) and its
execution stats come from running that plan.

#### Fixed

- `explain` of an aggregate whose filter holds `$exists` on one field matches
  mongod 8.2.11. RM-071 in part (tests/mongod_live/test_sbe_filtered_front_explain.py::test_filtered_front_explain_matches_mongod).
