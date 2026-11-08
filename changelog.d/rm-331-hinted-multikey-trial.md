### A hint to a multikey index still runs mongod's plan trial

Over a multikey index, two range predicates on one field cannot be intersected, because different array elements may satisfy each one. mongod builds one plan per predicate and keeps the one that wins its trial. A hint narrows that choice to the hinted index's plans, but mongod still runs the trial between them. The Rust MongoDB server ran no trial for a hinted query and always bounded the scan by the first predicate. So `{s: {$gte: "aé", $lte: "\U0010ffff"}}` hinted to a multikey `s_1` scanned `["", "\U0010ffff"]` where mongod scans `["aé", {})`, and `explain` listed no rejected plans. Now a hinted query gets the hinted index's own candidates: one scan per predicate on its leading field, and their intersection when those scans are points. It never gets another index or a walk for the sort, and, as on mongod, a hinted plan is still never cached.

#### Fixed

- `explain` and the read order for a hinted query over a multikey index with several predicates on its leading field: the trial picks the bounds, and `rejectedPlans` lists the others.
- Fixes RM-331 (tests/backlog/test_rm_222_invalid_utf8.py::test_multikey_bounds_from_two_range_predicates); hinted cases added to tests/mongod_live/test_plan_trial.py::test_the_trial_explain_lists.
- Filed while working it, still open: RM-422, a hinted index that no predicate bounds sorts its FETCH filter by path, where mongod keeps the canonical order (tests/backlog/test_rm_found_2026_11_08_rm331.py::test_a_hint_that_bounds_nothing_keeps_the_canonical_filter_order).
