### The Rust server reported four spec errors under the wrong wrapper

mongod uses `Invalid $addFields :: caused by ::` for an expression that fails at
PARSE time, and the optimizer's or executor's prefix otherwise. The Rust
server's parse-time scanner already classified most of that correctly — but a
spec document missing a REQUIRED key fell through it, so `{$convert: {to:
"int"}}` and its siblings were reported as fold or runtime failures instead.

Now covered: `$convert` without `input`, `$dateDiff` without `startDate`, the
`n`-operator family (`$firstN` / `$lastN` / `$maxN` / `$minN`) without `n`, and
`$dateFromParts` with neither `year` nor `isoWeekYear`. The Python engine got
the same set in the preceding change.

**Ordering matters and is pinned by its own test.** mongod reports an
*unrecognised* key before a *missing* required one, so `{$firstN: {k: 1}}` is
"Unknown argument for 'n' operator: k" while only `{$firstN: {}}` is "Missing
value for 'n'". Both checks fire on the same document, so only their order
separates them — and getting it backwards changes the CODE on shapes that are
already correct, which is what happened on the Python side before it was
corrected.

**A note on how this was sized.** The backlog entry this closes claimed the Rust
server "never emits the stage wrapper at all", from a `grep` over two files. It
does: the wrapper, the wrapping-stage list, and a parse-time scanner covering
`$ifNull` and `$setEquals` — 76 of the 83 shapes the Python fix was worth — were
all already there. Running the scanner in a throwaway `#[test]` (possible
because `secantus-commands` is a clean-workspace crate needing no WiredTiger)
showed the true gap in one command. The entry has been corrected in place rather
than left beside a newer one.

#### Fixed

- `crates/secantus-commands/src/argtypes.rs`: `REQUIRED_SPEC_KEY` and the
  `$dateFromParts` year check, evaluated after every unknown-key table.

#### Changed

- Three tests in `argtypes`: the classification, the unknown-before-missing
  ordering, and valid specs that must fall through to folding.
