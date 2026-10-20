### Rust MongoDB server: six more query shapes read the index mongod reads

The order an unsorted `find` returns documents in now follows mongod
8.2.11's plan for six predicate shapes. The matched set was already right;
only the order differed.

#### Fixed

- `$ne: null` / `$nin: [null]` over an index holding arrays is a COLLSCAN,
  as mongod's planner refuses the index there.
- `$type: "object"` and `$type: "date"` scan their type's bracket of the
  index (`[{}, [])`, and every date), so a descending index returns the
  latest date first.
- `$elemMatch: {$ne: 5}` scans `[MinKey, 5), (5, MaxKey]`.
- A regex holding an alternation (`^s|t`) is bounded by every string, as
  mongod's `simpleRegex` bounds it. The prefix `s` was wrong.
- A partial index is used when a clause inside a top-level `$and` implies
  its filter. The check looked only at the query's top level. This change
  is in the shared `secantus-storage` crate, whose router uses the same
  check; `tools/probes/index_result_sets.py` finds 0 divergent of 1,689.

Fixes RM-155 (the six tests in `tests/backlog/test_rm_order_shapes.py`).
`tools/probes/find_order.py` and `tools/probes/explain_commands.py` differ
from mongod on the same replies as before (62 known top-k ties, RM-049; 22
explain replies).
