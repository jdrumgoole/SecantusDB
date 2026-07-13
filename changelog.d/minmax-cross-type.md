### `$min` / `$max` compare by BSON order — no more traceback leak (both servers)

The `$min` and `$max` update operators now compare the incoming value against the
current field value by MongoDB's BSON canonical-type order, instead of Python's
native `<` / `>`. This fixes two bugs found by a three-way update differential
against real `mongod` 6.0:

- **A leaked traceback.** A cross-type compare — e.g. `{$max: {a: "str"}}` on a
  numeric `a` — raised a raw `TypeError` (`'>' not supported between 'str' and
  'int'`) that surfaced to the client. Now it orders like mongod: a string
  out-ranks a number, so `$max` sets `"str"`; `$max` of an ObjectId, a date, or a
  bool over a number likewise picks the higher-ranked value.
- **Explicit null treated as "no current".** An explicit-null field is a real
  value (BSON rank 2, below numbers), not an absent field. `{$min: {a: 9}}` on
  `{a: null}` now keeps `null` (null < 9); a genuinely *missing* field is still set
  unconditionally.

#### Fixed

- `update.py` / `secantus-core`: `$min`/`$max` use `ordering._bson_lt` (Python) /
  `order::cmp` (Rust) with a missing-vs-present split. The Rust engine handles the
  sortable subset (null / number / string / objectId / date / doc / array)
  natively and defers a bool / Decimal128 / NaN / exotic operand to the Python
  oracle (whose `_bson_lt` covers the full order).
