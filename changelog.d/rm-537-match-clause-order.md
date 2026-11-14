### Rust MongoDB server: explain lists a pipeline `$match` as written

A `$match` that explain lists after the cursor (behind a `$limit` or a
`$group`, after `$documents`, or moved ahead of a `$replaceRoot`) had its
clauses sorted as the query's `parsedQuery` sorts them. mongod 8.2 optimises
a pipeline `$match` without sorting it, so it lists the clauses in the
order they were written: `{d: {$gt: 0}, c: 1}` is `$and: [d, c]`, an
`$expr`'s `$_internalExprEq` comes ahead of the `$expr`, and
`{a: {$gt: 0, $lt: 5}}` is `[$gt, $lt]`. The new `match_as_written` in
`secantus-core` reproduces mongod's `ListOfMatchExpression` optimiser.
Children of a nested `$and` go after the node's own clauses, a node of one
child is that child, a `$nor` of one is its `$not`, duplicates and a `$nor`
among other clauses stay, and an `$or`'s equalities on one path become an
`$in` listed first. Where mongod's order comes from an unordered map (an
`$or` with two or more equalities alone on their path), it changes from one
mongod process to the next, and this server lists those in the order they
were written.

#### Fixed

- Fixes RM-537 (tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_a_pipeline_match_lists_its_clauses_as_written, tests/backlog/test_rm_found_2026_11_09.py::test_explain_lists_a_match_after_the_cursor_in_written_order), with `tests/mongod_live/test_match_stage_order.py` comparing 47 filters with mongod.
- Filed RM-660: a query's `parsedQuery` still takes an `$elemMatch` document with a `$`-key as the value form, keeps a `$not` of two operators whole, and keeps a `$nin: []` beside another clause.
