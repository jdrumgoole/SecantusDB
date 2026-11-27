### A count over an index scan with a fetch filter explains as mongod's SBE plan

`count` on a view, a `$count` or a `$group` that reads no field, whose
query an index scan of one interval serves with a `FETCH` filtering what
the index left, is now explained as mongod 8.2.11 explains it: `explainVersion`
"2", a `queryPlan` of `GROUP` over `FETCH` over `IXSCAN`, and a slot-based
plan of a `filter` over an `nlj` of an `ixseek` and a `limit 1` `seek` of
each document, executed for its statistics. `allPlansExecution` lists the
trial's classic plans, as mongod does.

#### Fixed

- Fixes RM-695 (tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view_fetching_from_an_index;
  tests/mongod_live/test_sbe_fetch_explain.py).
- Filed RM-831: the same plan over an index scan of more than one interval.
