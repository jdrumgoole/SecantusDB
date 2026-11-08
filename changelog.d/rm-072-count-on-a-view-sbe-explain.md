### `count` on a view is explained as the slot-based plan mongod runs

A `count` on a view is, inside mongod 8.2, an aggregate: the view's pipeline, the count's own query, skip and limit, then a `$count`. mongod pushes that `$count` into its slot-based engine behind the query, so `explain` answers with `explainVersion` "2", a `GROUP` over the collection scan and the slot program that runs it. The Rust MongoDB server answered with the classic cursor and a `$group` stage. It now builds the same plan, prints the same slot program and runs it to get the execution counts.

The same work extends the slot-based plans the server already built for a plain `$group`. A `$group` or `$count` behind a filtered collection scan now gets mongod's plan too: a `filter` stage of `traverseF` programs for comparisons and their `$and`s, and `limit` / `limitskip` stages for paging. Under a limit, the stages below it stop as mongod's do, and `totalDocsExamined` is what the scan actually read.

#### Fixed

- `secantus-commands` `explain/sbe/`: `count` on a view, and an `aggregate` whose `$group` or `$count` runs behind a filtered or paged collection scan, are explained as mongod 8.2.11's slot-based plan (`queryPlan`, `slotBasedPlan`, SBE `executionStats`). This covers comparisons (`$eq`, `$lt`, `$lte`, `$gt`, `$gte` on a number, string or boolean, dotted paths included) and their `$and`s, and a skip and limit when the group reads no field. The SBE stages are pull-based, so the `isEOF`, `numTested` and `numReads` under a limit match mongod's.
- `secantus-commands` `explain/sbe/`: the EOF plan over a collection that does not exist reports the `$cursor`'s `saveState` / `restoreState` when a classic stage follows it.
- `count` on a view over a collection scan is pinned by tests/mongod_live/test_sbe_filtered_front_explain.py. RM-072 stays open for a view whose filter an index serves (`COUNT_SCAN`, `IXSCAN` / `FETCH`), and RM-071 for the filters not built here (`null`, `$in`, `$ne`, `$or`).

Fixes RM-072 (tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view), in part.
