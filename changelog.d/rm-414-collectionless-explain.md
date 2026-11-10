### A collectionless aggregate's explain counts its stages and moves its `$match`

Explaining an aggregate that makes its own documents -- `{aggregate: 1}`
over `$documents`, `$queryStats`, `$listSampledQueries` or
`$listLocalSessions` -- now gives the answer mongod 8.2 gives. At
`executionStats` every stage carries its `nReturned`, counted the way
mongod counts it: a stage that streams into a `$limit` reports only what
the limit pulled, and a `$skip` reads past what it skips. The `$queue`
that `$documents` starts with shows that the run took its one document. A
`$match` behind `$documents` now appears ahead of its `$replaceRoot`,
rewritten over `_tempDocumentsField` in mongod's guarded `$or`. It stays
behind when it holds an `$expr` or an `$elemMatch`, or is only
`$alwaysFalse`. A `$skip` moves ahead of the stages that turn one
document into one, and `$listLocalSessions` lists the users whose sessions
it reads.

#### Fixed

- `explain` of a pipeline with its own source (`explain::own_source_reply`)
  reorders the stages after the source as mongod does
  (`hoist::hoist_after_source`: a `$match` moves through a `$replaceRoot`
  of a field path, a `$skip` through a single-document transform) and, above
  `queryPlanner`, counts each listed stage. The count runs the stages
  mongod runs `$documents` as, and adds the spill fields of `$sort` /
  `$group`.
- The per-stage counts in front of a `$limit` handle a `$skip`: it hands
  over what is pulled and reads that many more. This applies to aggregates
  over a collection too.
- Fixes RM-414 (tests/backlog/test_rm_found_2026_11_08b.py::test_a_collectionless_explain_counts_each_stage;
  regression tests/backlog/test_rm_found_2026_11_08b.py::test_a_collectionless_explain_reorders_and_counts_as_mongod
  and the `explain::hoist` unit tests).
- Filed RM-536 (the `$replaceRoot` rewrite over a collection), RM-537 (a
  pipeline `$match` listed with its clauses sorted rather than as written),
  RM-538 (optimizer rewrites mongod lists that this server does not) and
  RM-539 (statistics mongod gives `$facet`, `$unionWith`, `$lookup`,
  `$count` and `$sort` stages).
