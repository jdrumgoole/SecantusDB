### `$setWindowFields` rejects options it used to ignore

An unknown field in a `$setWindowFields` stage was accepted and dropped. A
caller who misspelled `partitionBy` got their accumulators computed over the
whole collection as a single partition, and one who wrote `range` at the top
level — where it looks plausible, but belongs inside a window — silently got the
default window covering the entire partition. Both returned `ok` with a wrong
answer rather than an error, which is the worst shape for an option: the caller
believes they asked for something.

The same silence applied one level down, to unknown keys inside `window`, where
a misspelled `documents` widened the window without saying so.

Both now match MongoDB: an unknown top-level field is `40415`, a missing
`output` is `40414`, and an unknown `window` key is `9`. Fixed on both servers.

Found by probing aggregation validation errors against MongoDB 8.2.11 while
working a filed item about error *codes* — the silent acceptance was the more
serious problem sitting next to it.
