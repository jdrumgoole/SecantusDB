### Arrays on the Rust PostgreSQL server, and the bug that correct types exposed

`int[]` is a different type from `int`, and the difference is easy to lose:
PostgreSQL's own parser keeps the array-ness of a declared type in a separate
`array_bounds` field rather than in the type's name, so code that reads only the
name types every array column — and every array cast — as its element type.

That mistake is quiet in a way worth recording. While a cast to `text[]` was
degrading to `text`, comparing two arrays rendered both sides to their text form
and compared the resulting strings, which agrees with PostgreSQL often enough to
look correct. Typing the casts properly is what revealed that array comparison
had never been implemented at all — so the feature that looked like a regression
was really a gap that the wrong types had been hiding.

Array NULLs then turn out not to follow scalar NULL rules, and all four rules
here were probed against a live PostgreSQL rather than reasoned out: inside an
array two NULLs compare equal, a NULL sorts after every non-NULL, a shared
prefix makes the shorter array the smaller one, and empty equals empty. Scalar
`NULL = NULL` is NULL, so an elementwise comparison written by analogy with the
scalar path gets every one of them wrong.

Multidimensional arrays are refused rather than answered. The encoder beneath
this handles a single dimension, and the flattening it produced turned
`{{1,2},{3,4}}` into two elements whose text read `{1,2}` and `{3,4}` — an
answer a client has no way to tell apart from a real one.

#### Added

- Arrays as column types, cast targets and literals: PostgreSQL's text form with
  its quoting rules, `NULL` elements, empty arrays, and the array type oids, so
  a client reads a list rather than a string.
- Array comparison and ordering, including the four NULL and length rules above.

#### Fixed

- An array cast lost its brackets, so `%s::text[]` was planned as `%s::text`.
- Array values were reported under their element type's oid, so `ARRAY[1,2,3]`
  arrived at the client as the string `{1,2,3}`.
- Comparing two arrays raised "comparing these operands" instead of comparing
  them.

#### Changed

- A multidimensional array now raises `0A000` instead of being silently
  flattened into its rendered inner literals.
