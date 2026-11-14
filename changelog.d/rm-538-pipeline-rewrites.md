### Rust MongoDB server: `explain` lists the pipeline rewrites mongod's optimizer makes

An aggregate's pipeline is now rewritten as mongod 8.2.11's optimizer
rewrites it, for the listing `explain` gives and for the run. A
`$replaceRoot` / `$replaceWith` of the document itself (`$$ROOT`,
`$$CURRENT`) is dropped. A `$sort` takes the `$limit` behind it as its own
`limit` -- over the stages that keep the number of documents (`$project`,
`$addFields`, `$replaceRoot`, `$lookup`) and over a `$skip`, whose amount
the limit then counts -- and `explain` lists the two as one `{$sort:
{sortKey, limit}}`, inside a `$facet` and after a `$rankFusion` or
`$scoreFusion` too. A `$match` on a field an `$addFields` / `$set` sets to
a field path of one component moves ahead of it onto that field, in an
`$or` and an `$expr` too. `$sampleRate` is parsed into the `{$expr: {$lt:
[{$rand: {}}, p]}}` mongod makes of it (`{$alwaysFalse: 1}` for 0, nothing
for 1), and a predicate that draws a random number no longer moves ahead
of any stage.

#### Fixed

- `explain` listed each of those stages as written: `{$replaceWith:
  "$$ROOT"}`, a `$sort` and its `$limit` as two stages, a `$match` behind
  `{$set: {b: "$a"}}` left behind it, and `{$sampleRate: 0.5}` as written.
  Fixes RM-538 (tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_the_pipeline_rewrites_mongod_lists,
  tests/backlog/test_rm_found_2026_11_08_explain_listing.py::test_sort_and_limit_after_the_cursor_are_one_stage);
  regression tests in tests/mongod_live/test_pipeline_rewrites_explain.py.
- `explain` constant-folded an `$expr` of `$rand` (`{$lt: [{$rand: {}},
  0.5]}` came out `$alwaysFalse` or nothing, drawn once), and moved it
  ahead of a stage mongod leaves it behind.
- Inside a `$facet`, `explain` listed a `$sort` and its `$limit` as two
  stages, at `executionStats` too.
  Fixes RM-685 (tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_sort_and_its_limit_in_a_facet_are_one_stage).
- Filed while working it: RM-662 (an empty `$match` or `{$alwaysTrue: 1}`
  behind a `$group`, and a `$match` behind a `$$ROOT.x` new root, each
  listed where mongod's optimizer does not leave it), RM-663 (`explain`
  of a `$sampleRate` the run refuses answers) and RM-689 (a `$sort` that
  took a `$limit` reports the size of every document it sorted, which was
  the rest of RM-685).
