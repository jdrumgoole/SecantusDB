### $bucket errors on an out-of-range value instead of silently dropping the document

`$bucket` did almost no validation. Worst of all, a document whose `groupBy` value
fell outside every bucket and had no `default` was **silently dropped** — silent
data loss. mongod errors (7158303). It now does too, on both servers.

`$bucket` also now validates the rest of its spec like mongod, instead of
silently accepting it: missing `groupBy` (40198), non-array `boundaries` (40200),
fewer than two boundaries (40192), boundaries of mixed type (40193) or not
strictly ascending / duplicated (40194), a `default` that falls inside the bucket
range (40199), and a non-document `output` (40196). The Python server carries
mongod's codes; the Rust core defers those cases to `BadValue`. Valid buckets are
unaffected. Three-way mongod 7.0.12-verified.

#### Fixed

- `$bucket` errors (7158303) on an out-of-range value with no `default` instead
  of silently dropping the document, and rejects an invalid spec (missing
  groupBy, bad/unsorted/mixed boundaries, in-range default, non-doc output) with
  mongod's codes instead of silently accepting it (both servers).
