### Rust MongoDB server: the plan cache holds the entries mongod holds

The classic plan cache keeps an entry only when mongod's planner enumerated more than one candidate plan, so which entries `$planCacheStats` lists depends on the candidates being counted as mongod counts them. Three shapes counted them wrongly. A `$not` of several operators (`b: {$not: {$gt: 3, $lt: 9}}`) is the negation of an `$and`, which no index serves; the explain simplifier rewrote it into an `$or` of negated leaves and offered `b_1` as a second plan, so this server cached an entry mongod does not. An object `$elemMatch` of several predicates parses to an `$and` of its own that the planner did not look into, so `d.e_1` was not offered and no entry was cached where mongod caches one. And an `$or` beside another clause was planned as the one combination each branch's own trial picked; mongod tries every combination of the branches' indexes (a branch's own index, or a compound index with the clause pushed down), up to ten, so `{$or: [{a: 3}, {b: 5}], c: {$lt: 3}}` over `a_1`, `b_1`, `a_1_c_1` and `b_1_c_1` now tries mongod's four plans and caches its entry.

#### Fixed

- `explain` planning: a `$nor` of one child no longer counts as a node when the Boolean simplifier compares sizes, so `{$not: {$gt: 3, $lt: 9}}` is left as mongod leaves it; the predicates under an object `$elemMatch`'s `$and` (and under a nested `$elemMatch`) are tagged for their indexes; an `$or` beside other clauses is enumerated in every combination of its branches' indexes.
  Fixes RM-826 (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_the_plan_cache_holds_the_entries_mongod_holds, tests/backlog/test_rm_found_2026_11_08_rm341.py::test_an_or_beside_a_clause_is_tried_in_every_combination_of_its_indexes).

#### Known gaps

- Two `$or`s in one `$and` are still planned through the first only (RM-851), and an `$or` with a compound branch lists more combinations than mongod (RM-852). The entries these two shapes cache are not sized (RM-694).
