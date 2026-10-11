### A partial TTL index no longer deletes what it does not hold, and three Rust MongoDB server gaps close

The Python MongoDB server expired documents outside a partial TTL index's
filter: three expired documents, one matching the filter, and all three were
deleted. It now deletes the one, as `mongod` 8.2.11 does.

The rest is the Rust MongoDB server. An `_id` is unique by the collection's
default collation, a `$out` that fails leaves its target alone, and a
time-series collection has its buckets.

#### Fixed

- **Python server: a partial TTL index deleted documents its filter leaves
  out.** It also expired documents by a compound index that `collMod` had
  given an `expireAfterSeconds`, and missed a date held in an array.
  `collMod` now refuses a TTL on a compound index or on `_id`, and a
  negative one (72).
- **Rust server: `_id` was not unique by a collection's default
  collation.** With `strength: 2`, `"A"` went in beside `"a"`. Insert,
  upsert, rename and a transaction all follow the rule now. A collection
  created with a collation before this release is not protected for the
  documents already in it.
- **Rust server: a `$out` that failed destroyed its target.** The target
  was dropped before the new documents went in, so a duplicate `_id` or a
  unique index the output broke left it empty or half-written. `$out` now
  builds beside the target and swaps it in. A duplicate `_id` in the output
  used to be dropped without an error; it is reported (11000).
- **Rust server: `$merge` matched by a simple comparison**, whatever the
  target's collation, and so inserted a second spelling of an `_id`. It
  matches under the target's collation and refuses to change `_id` (66).
- **Rust server: a duplicate key on a clustered collection** names no index
  and carries `foundValue`, as on `mongod`.

#### Added

- **Rust server: a time-series collection's buckets.**
  `system.buckets.<name>` and `system.views` can be read and are listed.
  A scan returns measurements bucket by bucket with the time and meta
  fields first. `validate`, `collStats` and `$collStats` report buckets.
  The server still stores measurements, not buckets, and derives the
  buckets on request; `tasks/backlog.md` section 7.071 has what that leaves
  out.
- **Rust server: `convertToCapped`, `cloneCollectionAsCapped`, `compact`
  and `reIndex`.** They answered `no such command`.

#### Changed

- **Rust server: `validate`** reports `repairMode`, the collection's `uuid`
  and each index's `spec`, in `mongod`'s field order.
- **Rust server: `$out` to a capped collection is refused** (17152).
