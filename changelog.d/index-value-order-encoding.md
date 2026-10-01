### Indexes order embedded documents and arrays by value

An index keyed a document or an array by its raw BSON, which begins with the
value's length, so byte order was size order: `{a: 2, b: [3]}` sorted above
`{a: 5}` because it is longer. The Rust server refused to use an index for a
range or sort over such values; the Python server used it anyway and scanned
the wrong part of the index. Both servers now key documents and arrays the way
mongod compares them, element by element, and use the index.

#### Changed

- **Index entry format 4.** A store whose indexes were written by an earlier
  build is refused at open with `IncompatibleStorageFormatError`; drop and
  recreate the indexes (the documents themselves are untouched -- `_id` keys
  keep their encoding).
- Range queries, partial-index use and sorts over document-valued fields use
  the index on the Rust server again; on the Python server they now return the
  right documents.
- Strings inside documents and arrays follow the index's collation, as they do
  in mongod.

#### Added

- `tools/probes/value_order_encoding.py`: the index encoder against mongod's
  `$cmp` over 16,110 random pairs.
