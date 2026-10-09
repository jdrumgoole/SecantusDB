### Multi-stage aggregations on the Rust MongoDB server decode fewer fields

The Rust MongoDB server reads only the fields a `$group` needs from each
document, but it did so only when `$group` was the first stage after a leading
`$match`. A pipeline such as `$match`, `$unwind`, `$group`, `$sort` decoded
every field of every surviving document first.

The server now works out which fields the leading stages read, through
`$unwind`, `$sort`, `$match`, `$skip` and `$limit`, up to the first `$group`,
`$sortByCount` or `$count`, and decodes only those. On a Mac, over 10,000
five-field documents, the benchmark's multi-stage pipeline went from 13.1 ms
to 10.1 ms, and `$match` then `$count` from 6.2 ms to 3.0 ms; `mongod` 8.2.11
takes 4.6 ms and 1.4 ms. Wider documents gain more. Pipelines with any other
stage in front of the `$group` are decoded whole, as before.

#### Changed

- `aggregate` on the Rust MongoDB server decodes only the fields read by a
  pipeline's leading `$unwind` / `$sort` / `$match` / `$skip` / `$limit`
  stages and the `$group`, `$sortByCount` or `$count` they lead to.
