### A sort with a limit spills as mongod's top-k sorter does

A sort that keeps only its first few documents still has a memory budget in
mongod, and over it mongod spills what it holds just as a whole sort does.
Its top-k sorter writes a run each time it passes the budget, sets a cutoff
from those runs that turns away documents no better than ones it already
kept, and stops every merged run at the limit. The Rust MongoDB server now
runs that sorter, so the profile, `explain` and `serverStatus` report the
same spills, spilled records and sorted bytes as mongod 8.2.11 for a `find`
or a pipeline whose `$sort` has a limit, whatever order the documents
arrive in. Before, such a sort never spilled.

A pipeline's `$sort` also takes a `$limit` that comes after an `$addFields`,
`$project` or `$skip` the way mongod's optimizer moves it in, so
`[$sort, $addFields, {$limit: 50}]` sorts as a top 50.

#### Fixed

- `secantus-core` `sorter`: a sort with a limit above one runs mongod 8.2's
  `TopKSorter`. It spills at the budget, moves its cutoff from the worst key
  and the median of what it spilled, counts as sorted only the documents it
  added while holding fewer than the limit, stops merged runs at the limit,
  and refuses with 292 when it would spill without `allowDiskUse`.
- `secantus-core` `aggregate`: `$sort` takes the least `$limit`, plus the
  `$skip`s before it, past stages that hand on each document
  (`extractLimitForPushdown`). `swaps_with_skip_and_limit` moved here from
  `secantus-commands`.
- `secantus-commands`: the read in front of a pipeline whose leading `$sort`
  has a limit keeps the plan's input to the sort, in the plan's order, so the
  sorter counts what mongod's does. The pipeline's own documents are
  unchanged.

Fixes RM-399 (tests/backlog/test_rm_326_sort_counts.py::test_top_k_sort_spills)
Regression tests: tests/mongod_live/test_top_k_sort_spills.py::test_top_k_sort_spills_as_mongods,
`secantus-core` `sorter::tests::a_sort_with_a_limit_spills_as_mongods_top_k_sorter_does`
and `aggregate::tests::a_sort_takes_the_limit_mongod_pushes_into_it`.
