### A `find`'s filter is Boolean-simplified, as mongod 8.2 does it

mongod runs a Boolean expression simplifier over the filter of every query
(`internalQueryEnableBooleanExpressionsSimplifier`, on by default), so
`explain`'s `parsedQuery`, and the query shape, plan cache key and plan
choice that follow from it, are those of the simplified filter. The Rust
MongoDB server had no simplifier and printed each filter as written.

#### Added

- `{$or: [{a: 1, b: 1}, {a: 1}]}` and
  `{$and: [{a: 1}, {$or: [{a: 1}, {b: 2}]}]}` are `{a: {$eq: 1}}`;
  `{a: 1, $nor: [{a: 1}]}` is `{$alwaysFalse: 1}`; an `$or` that names a
  predicate both ways is always true (`{}`); a repeated predicate is kept
  once; terms that differ in one predicate merge (`a & b` and `a & !b` are
  `a`); the predicates every term of an `$and` shares come out of its
  `$or`. The rules are the ones measured against mongod 8.2.11
  (`crates/secantus-commands/src/explain/simplify.rs`): a disjunctive
  normal form at every node over a bitset per term, Quine-McCluskey
  without Petrick (a term a merge made does not absorb its neighbours), the
  result used only when it is smaller, and the tree left alone past 64
  predicates or 1,000 terms. A pipeline `$match` stage is not simplified, as
  on mongod.
- The unwind filter of a change stream (`internalUnwindTransaction`) goes
  through the simplifier too: after `operationType: "drop"` (`rename`,
  `dropDatabase`) the writes branch and the event-type `$or` fall away.

#### Fixed

- A `parsedQuery` no longer drops a repeated predicate from a tree mongod
  leaves as written (a filter it does not simplify keeps its duplicates),
  and lists the clauses of a filter as mongod's optimiser orders them: the
  children of a nested `$and` after the parent's own, a `$not` by what it
  negates, and a `$nor` after the other predicates. An `$or` of one value's
  equalities beside no other child is that equality, and beside another
  child an `$in` of one value.
- Fixes RM-774 (`tests/backlog/test_rm_found_2026_11_23_rm699.py`) and
  RM-699 (`tests/backlog/test_rm_found_2026_11_15_rm452.py::test_the_unwind_filter_is_simplified`).
  Compared with mongod in `tests/mongod_live/test_boolean_simplifier.py`
  (27 filters and 300 seeded random trees); the residue is RM-778.
