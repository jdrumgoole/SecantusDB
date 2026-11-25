### A regex inside an `$in` on a change stream's namespace joins the oplog read

A `$match` after `$changeStream` with an `$in` on `ns.db`, `ns.coll`,
`to.db` or `to.coll` that holds regexes is rewritten into the oplog
filter as mongod 8.2 does: each oplog field matches the `$in`'s
equalities (an `$eq` when one is left) or one of its regexes' `$expr`s.

#### Fixed

- `explain` of such a stream reports mongod's `parsedQuery` and
  `queryHash` (7426DAC8 for `ns.coll: {$in: ["x", /^c/i]}`), and the
  unwind's filter carries the same predicate. Part of RM-698
  (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_regex_on_the_namespace_joins_the_oplog_filter).
