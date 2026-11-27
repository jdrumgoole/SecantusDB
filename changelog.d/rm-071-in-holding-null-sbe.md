### An `$in` holding null runs in SBE, as on mongod

An aggregate whose `$match` is an `$in` holding `null` on a one-field path,
in front of a `$group` or `$count`, now explains and runs as mongod 8.2.11's
slot-based plan: `isMember` of the value with Nothing and undefined folded to
null, against an `InList` the plan's environment binds. It was explained as
the classic `$cursor` before.

#### Fixed

- Trims RM-071 (tests/mongod_live/test_sbe_filtered_front_explain.py::test_filtered_front_explain_matches_mongod);
  a dotted `$in` holding null, `$exists` and a field-reading `$group` behind
  a `$limit` stay open (tests/backlog/test_rm_057_084.py::test_explain_of_a_filter_sbe_runs_beyond_comparisons).
