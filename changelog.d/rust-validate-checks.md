### Rust MongoDB server: `validate` checks the collection

`validate` on the Rust MongoDB server answered `valid: true` without
reading anything, and reported every index as holding one key per document.
It now reads every document, compares each index with the entries its
documents should have, in both directions, and checks that the `_id` index
leads to each document and to none that is gone.

#### Fixed

- `validate` answers `valid: false`, with the counts in `errors` and the
  index marked in `indexDetails`, when a document cannot be read, an index
  entry is missing, or an entry belongs to no document.
- `keysPerIndex` is the number of keys the documents give the index, as
  mongod 8.2.11 counts them: an array gives one per distinct element.

#### Added

- `Storage::check_collection`, the check itself, with tests over a
  collection the server wrote and one damaged by hand.
