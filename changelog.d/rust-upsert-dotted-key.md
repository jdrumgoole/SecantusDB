### The same upsert, run twice, inserted two documents

On the Rust server, an upsert whose filter used a **dotted equality** stored a
document with a literal dotted key. `update({"a.b": 5}, {$set: {z: 1}},
upsert: true)` inserted `{"a.b": 5, "z": 1}` — a document mongod cannot produce,
and one that does not match the query that created it. So running the *same*
upsert again inserted a *second* document. An idempotent upsert is the canonical
use of the feature, and it was silently broken: no error, just a growing pile of
near-duplicate rows. mongod builds the nesting, storing `{a: {b: 5}}`, and
matches it on the next call.

The Python server has used a path-aware seed here since it hit the same bug; the
Rust port kept a plain insert. This is the "user-supplied path used as a dict
key" shape the project's own notes call out, and it is the third instance of it.

The same batch gives the Rust upsert mongod's field order. mongod emits `_id`
first, then the fields seeded from the query, then the fields the update added,
sorted by name; the Rust path had no ordering at all and emitted them in
insertion order. BSON field order travels on the wire and drivers compare raw
bytes, so this was visible to clients. Deliberately *not* imitated: mongod's
order for the query-seeded fields is an internal hash order that varies between
runs for identical input, so both servers sort those instead — an approximation
the Python server already made, and one the two servers now share exactly.

#### Fixed

- `secantus-storage`: an upsert seeded from a dotted equality builds the nested
  document mongod builds, so the upserted document matches its own filter and the
  operation is idempotent again.
- `secantus-storage`: an operator upsert emits mongod's field order (`_id`, the
  query-seeded fields, then the update-added ones sorted by name); a replacement
  upsert keeps the document's own order.
- `secantus-storage`: the leftover `any(k.starts_with('$'))` form test that
  selects the oplog entry's shape now uses the shared `is_operator_form`
  predicate, so it cannot drift from the engine's rule.
- `secantus-core`: `update::set_document_path` is the public, properly-typed way
  to build a dotted path, replacing a bare `Result<(), ()>` on the crate surface.
