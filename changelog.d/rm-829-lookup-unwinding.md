### `explain` lists a `$unwind` of a `$lookup`'s `as` field inside the `$lookup`

As mongod 8.2.11 does, an aggregate explain now folds a `$unwind` of the
`as` field into the `$lookup` before it, as `unwinding:
{preserveNullAndEmptyArrays[, includeArrayIndex]}`, with the unwound
documents' counts.

#### Fixed

- Fixes RM-829 (tests/backlog/test_rm_lookup_sbe.py::test_an_unwind_after_a_lookup_is_listed_inside_it)
