### An index could change which documents a query returned

Four defects in the index layer, three of them silent data loss: the query came
back with fewer documents than the collection held, no error, and the same query
on the same data returned everything as soon as you dropped the index. All four
were found by running SecantusDB and MongoDB 8.2.11 side by side over randomised
documents and diffing the result sets, and all four are now covered by that
sweep plus a regression suite.

A **sparse index** omits documents that are missing the indexed field — but a
query for `null` MATCHES them, because MongoDB's query language treats an absent
field as null. The planner used the index anyway, so `find({a: null})` on a
collection with a sparse index on `a` skipped every document that had no `a` at
all. The same applied to `$in` lists containing null, and to any sort: a sort
walks the whole index, so a sparse one truncated the result set outright. A
sparse index is now only used when some indexed field carries a predicate that
guarantees the field is present — and "could match a missing field" covers any
comparison against `null`, not only `$eq`: `{a: {$lte: null}}` matches an absent
field too, which the first version of the gate missed.

A **compound sparse index** was under-populated. MongoDB indexes a document that
has *at least one* of the indexed fields, keying the missing ones as null; we
required *all* of them, so `{a: 1}` (no `b`) never reached a sparse `{a: 1, b:
1}` index and `find({a: 1})` lost it.

A **partial index**'s "does this query imply the filter?" check compared values
in BSON sort order, where a string sorts above every number. MongoDB's range
operators are type-bracketed — `{$gt: 0}` matches numbers and nothing else — so
the check concluded that `{b: "x"}` implied `{b: {$gt: 0}}`, used an index that
does not contain those documents, and `find({a: 5, b: "x"})` returned nothing.

Finally, a query naming only fields covered by a partial index's filter (`{b:
5}` against an index on `a` partial on `{b: {$gt: 0}}`) left no key prefix to
pin, built an empty lookup key and raised `IndexError` out of the command
handler — which reached the client as an internal error rather than an answer.

#### Fixed

- `storage.py`: a sparse index holds an entry for any document with at least one
  of its indexed fields (`_sparse_covers`), matching MongoDB's compound-sparse
  rule. An index built before this change under-indexes until it is dropped and
  recreated; nothing rewrites existing entries.
- `storage.py`: the index pickers refuse a sparse index for a query whose
  predicates could match a missing field (`_sparse_index_usable` /
  `_predicate_may_match_missing`), including the sort-acceleration pickers.
- `storage.py`: partial-filter implication is type-bracketed
  (`_op_implies_bound`), so a cross-type comparison no longer claims coverage
  the index does not have.
- `storage.py`: a query fully covered by a partial index's filter scans that
  index instead of crashing.
