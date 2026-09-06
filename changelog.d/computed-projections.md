### `find` evaluates a computed projection field, instead of dropping it

`find({}, {total: {$multiply: ["$price", "$qty"]}})` returned documents with no
`total` in them — and reported success. The client asked for a computed field,
was told `ok: 1`, and got documents without it.

Computed projections are now evaluated per document. Almost every rule involved
had to be measured against a real server rather than reasoned about, and the
ones that were reasoned about first were all wrong:

- **Only a BSON number or bool is an include/exclude flag.** A string, `null`,
  an array, a date, an ObjectId, BinData, a regex, a Timestamp, MinKey/MaxKey
  are *literal constants* that replace the field on every document.
  `Decimal128("1.5")` includes a field and `Decimal128("0")` excludes one —
  `bool()` of either is `True` in Python, so the obvious test is backwards.
- **A plain sub-document is a sub-projection, classified per leaf.**
  `{o: {p: 1, z: "$b"}}` returns the *stored* `o.p` alongside the computed
  `o.z`, so the sub-document cannot be classified as a whole.
- **A computed `_id` replaces the stored one and moves to the end**:
  `{_id: "$b", a: 1}` gives `{a: …, _id: …}`.
- A bare field **reference** that resolves to nothing **omits** the output
  field, while an **expression** over a missing field yields **null**.
- An evaluation failure is an execution-time error carrying the namespace, the
  way mongod reports a per-document failure: `$size` on a non-array is
  `Executor error during find command: <db>.<coll> :: caused by :: …`.

Mixing a computed field with an exclusion has **three** different errors,
picked by the first offending field in spec order: `31253` for an inclusion
flag, `31310` for a literal (with the value rendered as a BSON debug string,
`n: [ 1, 2 ]`), and `31252` for an operator expression. Swapping two fields in
the spec swaps which code comes back. An empty sub-document is its own error,
`51270`, naming the leaf.

A test in `tests/test_projection.py` asserted that `{_id: None}` and `{_id: ""}`
were *includes*, under a docstring reading "Oracle-pinned against real mongod".
Nothing in that file can reach a mongod, so the claim had never been run — the
server returns the constant. Corrected in place. The Rust engine carried the
same wrong rule, with the same comment, and the projection parity suite had been
green throughout because the two engines agreed with each other on the wrong
answer — the "parity is not correctness" shape, caught here by the parity suite
turning red only *after* the Python side was corrected.

Unchanged, and deliberately: the Rust server still answers `2 BadValue:
projection is not supported by the Rust server`. Refusing is the better of the
two behaviours when the feature is absent, and the two servers are now
honest-refusal versus correct rather than honest-refusal versus silently wrong.
The Rust half is filed with the same measured semantics.

#### Fixed

- `secantus.projection`: an expression-valued projection field is evaluated per
  document; the spec is flattened to dotted leaves so a sub-document is
  classified per leaf and errors name the field mongod names; the flag/literal
  split follows the BSON type rather than Python truthiness.
- `secantus-core`'s projection engine defers a non-numeric `_id` spec value
  instead of returning the stored `_id`, so the two engines agree on mongod's
  answer rather than on each other's.
- `secantus.commands`: a computed projection's evaluation failure is wrapped as
  `Executor error during find command: <ns> :: caused by :: …`, which needs the
  namespace only this layer knows.
