### A regex on a change stream's namespace joins the oplog read

A `$match` after `$changeStream` with a regex on `ns.db`, `ns.coll`,
`to.db` or `to.coll` is rewritten into the oplog filter as mongod 8.2
does: an `$expr` of `$regexMatch` over the part of each oplog field that
names the namespace. A regex on a whole `ns` or `to` matches nothing.

#### Fixed

- `explain` of such a stream reports mongod's `parsedQuery` and
  `queryHash` (EE5C1E5A for `ns.coll: /^c/`), and the unwind's filter
  carries the same predicate. Part of RM-698
  (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_regex_on_the_namespace_joins_the_oplog_filter).
