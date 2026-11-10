### A `$lookup` without `from` runs the documents its pipeline makes

A `$lookup` can make its own documents instead of reading a collection: when
its pipeline starts with `$documents`, it needs no `from`, and each outer
document joins whatever that pipeline produces, with `let` variables bound and
`localField` / `foreignField` filtering the made documents as they would a
collection's. The Rust MongoDB server used to refuse every such stage with a
generic "requires 'from'" error; it now runs them as mongod 8.2 does, and
refuses one whose pipeline does not start with `$documents` with mongod's own
error.

#### Fixed

- A `$lookup` with no `from` whose pipeline starts with `$documents` runs,
  over a collection or in `{aggregate: 1}`, nested inside another `$lookup`,
  `$unionWith` or `$facet`, with `let`, and with `localField` /
  `foreignField` matched right after the `$documents` (ahead of the rest of
  the pipeline). Explain shows it reading `$cmd.aggregate`, with its fields
  in mongod's order.
- Every `$lookup`, at any depth, is now read before any stage's fields are
  checked, in mongod's order: a `from` that is not a string (9, "$lookup 'from'
  field must be a string, but found <type>") or is empty (73, "invalid $lookup
  namespace: <db>"), a `pipeline` that is not an array of documents (14), no
  `pipeline` without `from` (9), and a pipeline without `from` that does not
  start with `$documents` (9, "$lookup stage without explicit collection must
  have a pipeline with $documents as first stage"). `from: ""` used to run
  and join nothing, and a nested `$lookup` without `from` in a `$facet` or
  `$unionWith` used to run and join nothing too.
- Fixes RM-413 (tests/backlog/test_rm_found_2026_11_08b.py::test_a_lookup_without_from_runs_its_documents).
  Also pinned: `tests/mongod_live/test_collectionless_lookup.py` (25 cases
  against mongod 8.2) and `lookup_is_read_in_mongods_order` in
  `secantus-commands`.
- Backlog, found while working it: RM-528 (`$lookup`'s `let` type and a lone
  `localField` / `foreignField`), RM-529 (an empty-array `localField` joins
  nothing, where mongod joins it as null), RM-530 (`$documents` in a `$facet`
  is not refused with 40600) and RM-531 (`$documents` of a non-array answers
  2, not 5858203).
