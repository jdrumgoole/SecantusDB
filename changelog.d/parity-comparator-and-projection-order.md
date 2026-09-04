### Projected fields came back in the wrong order — in both engines, differently

mongod emits projected fields in the **stored document's** order and ignores the
projection spec's order entirely. Neither engine did that:

| | order emitted |
|---|---|
| mongod | document |
| pure Python | **spec** |
| Rust | **alphabetical** (its spec trie is a `BTreeMap`) |

So `find({}, {"c": 1, "b": 1})` returned fields in an order no mongod produces.
Field order is behaviour — a driver renders it, and a wire-level test can assert
it — and this was wrong on every projection either engine has ever done.

**Why it survived.** The parity suites compare the two engines, and `==` on a
dict ignores key order, so the dimension was invisible. The Rust variant was
invisible even to a probe for a subtler reason: alphabetical order *coincides*
with document order for any document whose keys happen to be sorted, and the
fuzz corpus was keyed `a, b, c`. Only a document keyed `z, a, m` separates the
three orderings, and nothing had one.

**The comparator is now shared.** `tests/parity_compare.py` holds the one
`same()` — NaN equals NaN, signed zeros are distinct, key order compared,
recursing into arrays and documents — and all eight `test_rust_*_parity.py`
suites use it. Previously only the expressions suite had a sharpened comparator,
and the other seven compared with `==`. Routing them through it is what surfaced
the projection bug within seconds.

#### Fixed

- `projection.py`, `crates/secantus-core/src/projection.rs`: `_include_doc` /
  `include_doc` iterate the DOCUMENT, not the spec trie. Nested levels follow
  their own sub-document's order. `exclude_doc` needed no change — it removes
  keys in place and so preserves order already.

#### Changed

- `tests/parity_compare.py` (new): the shared parity comparator.
- All eight parity suites route their value comparisons through it — 39 sites
  that used a bare `==`.
- Signed zero added to the update, projection and aggregate corpora, and the
  measured field-order cases to the projection corpus.
- `tests/test_projection_field_order.py` (new): pins the measured mongod order
  and explicitly excludes BOTH wrong answers, using documents that are not
  already alphabetical — the only shape that can tell them apart.
