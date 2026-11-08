### The Rust MongoDB server refuses a sort on parallel arrays, as mongod does

mongod builds a sort key with the same key generator it uses for a compound index, so it cannot sort a document whose sort paths reach two different arrays: `{$sort: {a: 1, b: 1}}` over `{a: [1, 2], b: [3, 4]}` is refused with code 2, "cannot sort with keys that are parallel arrays". The Rust MongoDB server sorted such documents anyway. Now it refuses them the way mongod does. That covers `find`, an aggregation `$sort`, `findAndModify`, an `update` with a `sort`, `$setWindowFields` and the `$top` / `$bottom` accumulators, and a collection holding only one matching document.

The rule is mongod's walk, ported: empty arrays count, two paths into one array of sub-documents are fine, and positional components such as `a.0` resolve for every element. This refusal and the ambiguous numeric-field refusal (16746) now come from one walk, in mongod's order. The `$top` family follows each engine's own rule. An `update` whose sort a document cannot be keyed for now reports mongod's writeError. Before, it dropped the error and updated an unsorted match.

#### Fixed

- `secantus-core::paths::sort_key_refusal`: a port of mongod's `BtreeKeyGenerator` array walk. It decides both the parallel-arrays refusal (2) and the ambiguous-index refusal (16746) in mongod's order, and it replaces the per-field `ambiguous_sort_path`. Both sort-key builders use it: the aggregation `$sort` (`aggregate::sort_key_values`) and `find`'s (`secantus-storage::sort_key`). The shared storage crate is linked by the Rust PostgreSQL server too, which does not call this path.
- `$top` / `$bottom` / `$topN` / `$bottomN`: refuse only when two or more `sortBy` paths meet an array. SBE then refuses what a `$sort` does. The classic engine refuses parallel arrays even when the paths share one array, and never raises 16746 (measured on 8.2.1).
- `update` with `sort`: a sort-resolution error is the statement's writeError, `Plan executor error during update :: caused by :: …`. It used to be discarded. A lone matching document now has its sort key built too: the planner's order path skipped the sort for fewer than two rows.
- `$setWindowFields`: the spec's own refusals (for example 5371602, a rank function or `$documentNumber` over a multi-field `sortBy`) are raised at parse time. They now come before the refusal of the `$sort` the stage runs behind.

Fixes RM-332 (tests/backlog/test_rm_222_invalid_utf8.py::test_a_sort_on_two_arrays_is_refused), pinned across every entry point by tests/mongod_live/test_sort_key_refusals.py::test_sort_key_refusals_match_mongod. Files RM-423 (an aggregate `$sort` on a `$meta` key is refused).
