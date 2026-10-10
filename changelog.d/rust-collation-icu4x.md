### Rust MongoDB server: collation that orders and matches as mongod does

The Rust server's collation was a hand-written approximation. Measured
against mongod 8.2.11 over 764 scenarios it gave a different answer in 462:
range queries compared code points, `$sort`, `$group` and every expression
ignored the collation, a collection's default collation was never used, and
a malformed collation document was accepted. It now compares through ICU4X,
the Rust implementation of the algorithm and locale data mongod gets from
ICU, and 8 scenarios differ.

Three bugs found beside it are fixed too. `renameCollection` dropped the
collection's options, so a validator stopped being enforced after a rename.
A unique index with a collation compared bytes, so it stored `"a"` and `"A"`
together at strength 2. And `$group`'s `$min` / `$max` kept the first of two
equal values where mongod keeps the last.

#### Added
- Collation by ICU4X: every `strength`, `caseLevel`, `caseFirst`,
  `numericOrdering`, `alternate`, `maxVariable` and `backwards`, and the 109
  locales mongod accepts with their `@collation=` variants (five variants are
  refused by name: `search`, `searchjl`, `big5han`, `gb2312han`,
  `phonetic`).
- A collation applies to aggregation expressions (`$eq`, `$cmp`, `$in`, the
  set operators, `$max` / `$min`, `$sortArray`, `$switch`), to `$group`,
  `$sortByCount`, `$bucket` and `$addToSet` keys, to the `$sort` stage,
  `$lookup`, `$graphLookup`, window and `$fill` partitions, and to the update
  operators `$pull`, `$pullAll`, `$addToSet`, `$push` with `$sort`, `$min`,
  `$max` and `arrayFilters`.
- A collection's default collation: stored in full, shown by
  `listCollections` and on the `_id` index, inherited by its indexes and by
  every command that names no collation. A view has its own.
- `$alwaysTrue` and `$alwaysFalse` in a query.

#### Fixed
- `renameCollection` keeps the collection's options and UUID.
- A unique index with a collation enforces uniqueness by that collation, on
  insert, update and build, and its partial filter is read under it.
- A malformed collation is refused on `find`, `aggregate`, `count`,
  `distinct`, `findAndModify`, `update`, `delete`, `create` and
  `createIndexes`, with mongod's code and message.
- `{locale: "simple"}` means no collation; it was read as one.
- `createIndexes` stores a collation spelled out as mongod does; two indexes
  on one key may differ by collation, and `dropIndexes` by that key answers
  181. An `_id` index with a collation other than the collection's is
  refused. `collMod` refuses an unknown `index` field.
- `$group` `$min` / `$max` keep the later of two equal values; `$maxN` lists
  the later of equal values first.
- A sort inside `$top` / `$bottom`, `$setWindowFields` and `$fill` reads an
  array by its smallest (or largest) element, as every other sort does.
- `$fill` with `locf` writes null where there is nothing to carry forward,
  and emits partitions in key order.
- `$setWindowFields` refuses an array partition key (14); `$group` refuses
  `_id: {field: 1}` (17390).

#### Changed
- An index created with a collation holds collation sort keys and is marked
  so in the catalog. A query with a collation still scans the collection.
  An index with a collation built by an earlier version keeps comparing by
  value until it is dropped and rebuilt.
