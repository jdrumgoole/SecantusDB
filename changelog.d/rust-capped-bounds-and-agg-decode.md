### Capped collections on the Rust MongoDB server hold their bounds within one insert

A capped collection on the Rust MongoDB server could grow past its cap. The
server never evicted a document that belonged to the batch being inserted, so
`insert_many` of five documents into a `max: 3` collection left all five, and
forty 500-byte documents into a `size: 4096` collection left all forty.
`mongod` 8.2.11 leaves three and seven. The server now evicts from the batch
like any other documents and always keeps the newest one, as `mongod` does. A
sweep of 34 capped-collection results (sizes, payloads, one batch against
single inserts, duplicates) went from 17 different from `mongod` to none.

Two changes make the same server faster. Aggregations decode fewer fields:
the server read only the fields a `$group` needs, but only when `$group` was
the first stage after a leading `$match`. It now works out which fields the
leading stages read, through `$unwind`, `$sort`, `$match`, `$skip` and
`$limit`, up to the first `$group`, `$sortByCount` or `$count`. And an insert
opens its `_id` index cursor once per batch, not once per document.

Measured on a Mac over 10,000 five-field documents, old and new binary
interleaved: the benchmark's multi-stage pipeline went from 13.1 ms to
10.1 ms (`mongod` 8.2.11: 4.6 ms), `$match` then `$count` from 6.2 ms to
3.0 ms (1.4 ms), and a 10,000-document `insert_many` from 48 ms to 43 ms
(38 ms).

#### Fixed

- A capped collection on the Rust MongoDB server is held to `size` and `max`
  after every insert, including for the documents of the batch that overflows
  it. The newest document is kept even when it alone is larger than `size`.

#### Changed

- `aggregate` on the Rust MongoDB server decodes only the fields read by a
  pipeline's leading `$unwind` / `$sort` / `$match` / `$skip` / `$limit`
  stages and the `$group`, `$sortByCount` or `$count` they lead to.
- `insert` on the Rust MongoDB server reuses one `_id` index cursor per batch
  and no longer collects the batch's keys.
