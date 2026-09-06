### A `MinKey` / `MaxKey` bound skipped documents that had no such field

`$gt` / `$gte` / `$lt` / `$lte` are **type-bracketed** — `{x: {$gt: 3}}` matches
numbers greater than 3 and nothing else. `MinKey` and `MaxKey` are the two
bounds that escape that bracketing, because mongod compares them against every
type, and an **absent** field is one of the things they must reach: mongod's
query language treats a missing field as `null`, which ranks above `MinKey` and
below `MaxKey`.

Both servers skipped an absent field outright in the range comparison, so
`{x: {$gt: MinKey()}}` and `{x: {$lt: MaxKey()}}` — the idiomatic "match
everything" bounds — left out every document that lacked the field entirely.

The fix is to compare an absent field as `null` rather than skip it, which is
safe for the ordinary bounds precisely *because* they are bracketed: `{x: {$gt:
3}}` then compares `null` against a number, the brackets differ, and the
document is dropped exactly as before. Both halves are pinned, along with the
null bounds, whose own rule (`$gte` / `$lte` match null and missing, `$gt` /
`$lt` match nothing) is unchanged.

#### Fixed

- `secantus.query` / `secantus-core`: a range comparison treats an absent field
  as `null`, so a `MinKey` / `MaxKey` bound returns the documents that have no
  such field, as mongod does.

Filed while probing the same surface, and materially worse than the entry that
described it: computed projections (`{lit: {$literal: 7}}`) are unimplemented,
and the Python server drops the field **silently** — `ok: 1`, and a document
without the field the client asked for. The Rust server refuses honestly, which
is the better of the two behaviours. Eleven measured shapes are in the backlog
with the two semantics that are easy to get wrong.
