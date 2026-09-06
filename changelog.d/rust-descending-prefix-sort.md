### A descending sort put every prefix chain in ascending order

`sort({x: -1})` over `["", "a", "ab", "abc", "b"]` came back from the Rust
server as `["", "b", "a", "ab", "abc"]` — the empty string first, and the whole
`a`/`ab`/`abc` chain ascending, inside a descending result. mongod answers
`["b", "abc", "ab", "a", ""]`.

The cause is a good idea used one step too far. A descending column is stored in
the B-tree by **inverting its key bytes**, which is the only way to express a
direction where the storage engine sorts by raw bytes. The Rust server reused
that trick to build an in-memory sort key — and inversion is not a descending
comparator, because it does not reverse a **prefix** relationship. `""` encodes
to a strict prefix of `"a"`'s key, and a shorter byte string sorts first both
before and after inversion.

Direction is now applied when the keys are **compared** rather than by inverting
them, which needs no trick at all: prefix-shorter-first is exactly right
ascending, and its reverse is exactly right descending. Nothing persisted
changes, so no stored index is affected — a descending index gives the same
answer it always did, and the same answer as no index at all.

The Python server was never affected, because its in-memory sort goes through
`ordering.sort_docs` rather than the encoder. It shares the encoder, though, so
both definitions now carry the warning that the inverted form is for B-tree
placement only and must never be used to order values.

#### Fixed

- `secantus-storage`: `sort_key` emits ascending per-field parts and
  `compare_sort_keys` applies each field's direction, so a descending sort
  orders prefix chains correctly. Per-field direction in a compound sort is
  unchanged, as is `[]`'s place between MinKey and null.
- `secantus-commands`: the `min` / `max` cursor-bound comparison compared
  inverted keys and had the same flaw; it now compares ascending encodings and
  negates.
- `secantus.sortkey` / `secantus-core`: `encode_value_directed` documents that
  it is for physical B-tree placement only, with the measurement that shows why.
