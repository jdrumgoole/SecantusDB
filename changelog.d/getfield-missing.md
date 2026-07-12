### `$getField` on an absent field now resolves to missing, not null

Reading a field that doesn't exist with `$getField` used to hand back an
explicit `null`. Real MongoDB treats an absent field as the *missing* value —
and a `$project` or `$addFields` computed field that resolves to missing is
omitted from the output document entirely, rather than emitted as `null`. Both
SecantusDB servers now match that: `{$project: {r: {$getField: {field: "k",
input: "$sub"}}}}` over `[{sub: {k: 1}}, {sub: {j: 2}}, {}]` yields `[{r: 1},
{}, {}]` — the documents with no `sub.k` carry no `r` field at all. A field that
is present with an explicit `null` still returns `null` and is emitted, so the
missing-vs-null distinction is preserved.

The same change makes `$$REMOVE` behave correctly as a `$project` / `$addFields`
computed value: the field is dropped instead of leaking the internal removal
sentinel.

#### Fixed

- `expressions.py` / `secantus-core`: `$getField` returns the missing/`$$REMOVE`
  marker (not `null`) for a field absent from its input; on the Rust side that
  case defers to the pure-Python engine, keeping the parity harness green.
- `aggregate.py`: `$project` and `$addFields` computed fields that evaluate to
  the missing marker are omitted from the output (an existing `$addFields`
  target set to the marker is removed), matching mongod.
