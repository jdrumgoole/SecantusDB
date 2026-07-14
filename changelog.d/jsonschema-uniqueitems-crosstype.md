### `$jsonSchema` `uniqueItems` bridges cross-type numerics recursively (both servers)

`{$jsonSchema: {properties: {arr: {uniqueItems: true}}}}` now detects duplicate
array elements using MongoDB value equality, which treats int / long / double /
Decimal128 as equal when their values match — and does so recursively inside
sub-documents and sub-arrays. So an array like `[{a: 1}, {a: 1.0}]` is correctly
rejected (the two documents are equal), matching real `mongod` 6.0.

Previously only *top-level* scalar arrays collapsed cross-type numerics (`[1, 1.0]`
was already a duplicate); a cross-type-equal numeric nested inside a document or
array element (`[{a: 1}, {a: 1.0}]`) was wrongly treated as distinct on both
servers, because duplicate detection keyed off a raw BSON encoding that differs for
int `1` versus double `1.0`.

#### Fixed

- `query.py` / `secantus-core`: `uniqueItems` duplicate detection uses a recursive
  canonical key (`_unique_items_key` / `unique_items_key`) that normalises numerics
  to a common value form at every nesting level and recurses into sub-documents and
  sub-arrays, instead of Python structural `==` or a raw sort-key/BSON encoding.
