### Aggregation no longer invents fields or empty buckets

Two aggregation results carried data MongoDB never sends.

A field path that doesn't resolve — `{$project: {z: "$nope"}}`, or `"$n.k"`
where `n` has no `k` — came back as `z: null`. MongoDB omits the key entirely.
The difference is invisible until code asks whether a field is present: `"z" in
doc` answered yes where a real server answers no, and every document in the
result carried an extra key. `$addFields` did the same, and a document literal
went further, turning `{$project: {z: {w: "$nope"}}}` into `{z: {w: null}}`
where MongoDB gives `{z: {}}`.

The rule being restored is narrower than "missing means null", which is why the
bug survived: a missing path *is* null when it's an argument to an operator —
`{$add: ["$nope", 1]}` is 1, not an error — and only *missing* when it's the
value of a projected field. Both behaviours are now pinned by tests so a future
fix to one can't quietly break the other.

Separately, `$bucket` emitted buckets that nothing landed in. An unused
`default` came back as a bare `{_id: "other"}` with no `count` field at all,
and empty boundary buckets were emitted too. MongoDB omits any empty bucket.

Both fixes landed on the Python and Rust engines together, checked against a
live `mongod`.

#### Fixed

- A field path that resolves to nothing is omitted from `$project` /
  `$addFields` output rather than emitted as `null`, including through nested
  document literals. Missing paths remain `null` as operator arguments, matching
  MongoDB.
- `$bucket` emits a bucket only when at least one document falls in it —
  boundary buckets and the `default` bucket alike.
