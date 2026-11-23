### A stored `undefined` is a value in aggregation expressions, not MISSING

The Rust MongoDB server's expression engine spelled MISSING `Bson::Undefined`, so
a real `undefined` read out of a document -- a field, an array element, a
variable bound from one -- was taken for an absent field. mongod 8.2.11 keeps the
two apart, and so does this server now: `$type` says `"undefined"` where it said
`"missing"`, `$addFields` and `$project` write the field instead of dropping it,
and `$group` keys it apart from null.

#### Fixed

- `{$type: ...}` of a stored `undefined` reads `"undefined"` as a field path, a
  path through `$$ROOT` or a variable, an element read by `$arrayElemAt`,
  `$first` or `$last`, a `$getField` result, what `$cond`, `$switch` and
  `$ifNull` return, a `$let` variable, and a `$map`, `$filter` or `$reduce`
  element. An absent field, and an index out of range, still read `"missing"`.
- `expressions::evaluate_field` returns a stored `undefined` as
  `Field::Value(Bson::Undefined)` beside `Field::Missing`, which `$addFields`,
  `$project`, a computed field of the document store, and `$shift` use.
- `$group` keys a stored `undefined` apart from null and missing (`GKey::Undefined`),
  as a scalar, inside a document key and as an element of an array key. An
  array key holding `undefined` was refused (code 2). `$sortByCount`, `$push`,
  `$first` and `$addToSet` keep it; `$min` and `$max` skip it as they skip null.
- A `$setField` value and a document literal's member that is a stored
  `undefined` is written, not removed.
- `{$meta: "sortKey"}` gives an empty array's key as `undefined`, not null.

Fixes RM-635 (tests/backlog/test_rm_found_2026_11_13_rm525.py::test_a_stored_undefined_is_not_missing,
tests/backlog/test_rm_found_2026_11_13_rm525.py::test_a_group_keeps_undefined_apart_from_null)
Fixes RM-525 (tests/backlog/test_rm_found_2026_11_10_rm410.py::test_an_empty_arrays_sort_key_is_undefined)
