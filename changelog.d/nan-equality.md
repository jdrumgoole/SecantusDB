### A document with `_id: NaN` can be found again

Writing `{_id: NaN}` succeeded and then the document was unreachable by its own
key: `find({_id: NaN})` matched nothing while the row sat in the collection. The
same held for any field — `{x: NaN}` never matched a stored NaN.

IEEE 754 says NaN is not equal to itself and Python and Rust both follow it, but
mongod matches `{x: NaN}` against a stored NaN. Storage was never at fault:
`sortkey.encode_value` already gives NaN a stable encoding, so the index entry was
correct all along. Only the equality matcher was wrong, on both servers.

The rule is confined to equality. Range operators and sort keep IEEE semantics, so
NaN still sorts below every other number and `$gt: NaN` still matches nothing.

#### Fixed

- `{field: NaN}` and `{_id: NaN}` match a stored NaN on both servers, across
  double and Decimal128 and between the two, matching mongod 6.0.16. Covered by
  `tests/test_nan_equality.py` and a `secantus-core` unit test that also pins the
  ordering behaviour left unchanged.
