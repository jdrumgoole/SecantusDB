### The plan cache works on collections of any size

mongod tries the candidate plans for a query against each other whatever
the collection's size, and caches the winner. The Rust MongoDB server used
to run that trial over the collection read whole into memory, so past 2,000
documents it skipped the trial: a fixed order of preference picked the plan
and nothing reached the plan cache, so `$planCacheStats` stayed empty on any
collection of a realistic size.

The trial now runs at any size, and reads only what mongod's trial reads:
for each candidate, the index entries its scans step through in the works
it takes, and the documents behind them, fetched from the store. A trial
that runs out of the entries it read is run again over eight times as many,
so its counts are the ones the whole collection gives, the 12,000-work trial
mongod gives 40,000 documents included. A small collection is still read
whole.

#### Fixed

- `secantus-commands` `explain/order.rs`, `plan.rs`, `trial.rs`,
  `stream.rs`: a trial over a collection of more than 2,000 documents runs
  over the documents its candidates' scans reach in the store
  (`stream::scan_reach`, `order::Lazy::reached`, `plan::over_reach`), with
  `internalQueryPlanEvaluationCollFraction`'s budget from the collection's
  real size; a query a sort-providing index walk competes for is tried at
  any size too. Fixes RM-342
  (tests/backlog/test_rm_340_342_plan_cache_gaps.py::test_a_collection_over_2000_documents_is_cached),
  with `tests/mongod_live/test_plan_cache_large.py` comparing seven query
  shapes' entries with mongod 8.2.11 over 40,000 documents.
- Filed while measuring it: RM-433 (a compound scan whose second field has
  several intervals examines a key more than mongod's) and RM-434
  (`explain`'s winning plan has no per-stage work accounting).
