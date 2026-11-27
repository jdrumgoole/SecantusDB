### A `$lookup` on an indexed foreign field is explained as an indexed loop join in SBE

`explain` of a `$lookup` on `localField` / `foreignField` whose foreign field
leads an index now reports what mongod 8.2.11 does: `explainVersion` "2", an
`EQ_LOOKUP` of strategy `IndexedLoopJoin` that names the index (`indexName`,
`indexKeyPattern`) over the query's plan, and the slot-based plan it runs. For
each local row it unwinds the join keys into the intervals of the index
(`ks` KeyStrings, a `shardHash` first for a hashed field), `ixseek`s them,
drops the documents a multikey index returns twice (`unique_roaring`), `seek`s
the documents and keeps those whose own keys match (`filter` of `traverseF`,
`group`). `executionStats` counts every seek, key and document of a run over
both collections and the index's entries, down to a unique index's point seek
reading no key past it.

The index is the one mongod picks: of those that lead with the field and are
btree or hashed, not sparse, partial or hidden, the one without a collation,
then with the fewest fields, a btree before a hashed one, then the lesser key
pattern; the `_id` index counts. A collated, sparse or partial index does not
qualify and the `$lookup` stays a hash join (a collated one is still named in
its `queryPlan`).

#### Fixed

- A `$lookup` on an indexed foreign field was explained as a classic
  `$cursor` and `$lookup` stage. Closes the indexed-loop-join slice of RM-683
  (tests/backlog/test_rm_lookup_sbe.py::test_a_lookup_on_an_indexed_foreign_field_is_an_indexed_loop_join,
  tests/mongod_live/test_lookup_indexed_loop_join.py); the nested loop join
  stays open under RM-683, and what the work found is filed as RM-838
  (decimal keys) and RM-839 (a `$lookup` under a collation).
- A `$lookup` with `allowDiskUse: false` over a collection no index serves was
  explained as a hash join, which needs `allowDiskUse`; mongod joins it by a
  nested loop (RM-683), so it is explained as the classic stage until that is
  built.
- The `makeObj` of a `$lookup` in front of another was not evaluated, so the
  second join's local keys were those of a missing field.
- A `)` or `,` that starts a line of an SBE expression takes back one column
  of the indent wherever it sits, not only at the stage's own indent.

#### Added

- The `unique_roaring` SBE stage, the `ixseek` over a join's runtime
  intervals, the `ks`, `shardHash` and `aggConcatArraysCapped` expressions'
  text, and `explain/sbe/index_join.rs`.
