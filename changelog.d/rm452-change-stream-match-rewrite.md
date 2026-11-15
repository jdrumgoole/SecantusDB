### Rust MongoDB server: a `$match` on `operationType` after a change stream joins the oplog read

The `explain` of a change stream followed by a `$match` on `operationType`
now prints the oplog filter mongod 8.2 runs. mongod rewrites the predicate
into the oplog entry's terms (`insert` is `op: "i"`, `update` is `op: "u"`
with no `o._id`, `drop` a command with `o.drop`, ...) and adds it to the
read's filter and to `internalUnwindTransaction`'s. This server used to
list the `$match` after the read and leave the filter unchanged, so the
`parsedQuery`, `queryHash` and `planCacheKey` differed.

#### Fixed

- The `operationType` rewrite follows mongod's
  `change_stream_rewrite_helpers.cpp`: `$eq`, `$in` (an `update` with a
  `replace` fused to `op: "u"`, CRUD types first), `$ne` / `$nin` /
  `$not` / `$nor`, unrewritable predicates left out of an `$and`, a whole
  `$or` left out when any branch cannot be rewritten, an unknown type as
  `$alwaysFalse`, and a subfield of `operationType` decided as a missing
  field. The event-type `$match` a stream without `showExpandedEvents`
  starts with now goes through the same rewrite rather than a fixed copy
  of its result. The filter's optimizer folds `$alwaysTrue` /
  `$alwaysFalse` and merges an `$or`'s equalities on one path into an
  `$in`, as mongod's does. A `$match` holding `$alwaysFalse` is listed as
  `$alwaysFalse`, and a one-child `$nor` (a `$not` of a sub-tree) hashes as
  a NOT node.
- Measured on 43 `$match` shapes against mongod 8.2.11. Filed as follow-ups:
  the rewrites of the other event fields and of `$expr` (RM-698), and the
  Boolean simplifier mongod runs over the unwind filter (RM-699).

Fixes RM-452 (tests/backlog/test_rm_found_2026_11_09_rm076.py::test_a_match_on_an_event_field_joins_the_oplog_filter)
