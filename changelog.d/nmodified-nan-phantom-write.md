### A document holding a NaN was rewritten by updates that touched nothing

The write guard asked "did this document change" with a value comparison, and
`NaN != NaN`. So an update that changed nothing at all — an `$unset` of a
missing field, a `$rename` of a missing source, a `$pull` that matched nothing —
looked like a change on any document containing a NaN *anywhere*, nested in a
subdocument or inside an array included. The document was rewritten, an oplog
entry emitted, a change-stream event delivered to every watcher, and
`nModified` came back 1. mongod reports 0 and writes nothing.

The Rust server carried this in full. The Python server was hidden from it by an
accident — container equality short-circuits on object identity, so an untouched
value compared equal — which was never a safe thing to rely on and is now gone
from both. Both servers compare the encoded BSON, which sees a signed zero and a
numeric type change while treating two identical NaNs as identical.

The encoding is not quite all of mongod's rule, and the remainder is the
interesting part. mongod counts an *arithmetic* write whose result is a NaN:
`{$inc: {a: 1}}` over `a: NaN` reports 1, while `{$min: {a: 5}}` over the same
document reports 0 because `$min` declined to write, and `{$set: {a: NaN}}`
reports 0 because it wrote an equal value. All five have byte-identical before
and after images, so no document comparison can separate them — the
discriminator is exactly "an arithmetic operator produced a NaN", and that is
now its own narrow check rather than a side effect of how `!=` treats NaN.

#### Fixed

- `secantus.storage` / `secantus-core`: the write guard compares the encoded
  BSON, so an update that touches nothing no longer rewrites a NaN-holding
  document, emits an oplog entry, or fires a change-stream event.
- `secantus.update` / `secantus-core`: `arith_wrote_nan` carries mongod's
  per-operator half of `nModified`, so `$inc` / `$mul` over a NaN still count as
  modifications while `$min` / `$max` that decline to write, and `$set` of an
  equal value, do not.
- `secantus-core`: `diff::same_encoding`'s catch-all compares values rather than
  BSON variant discriminants, which was only sound while it was called behind an
  equality check.
- `secantus.update`: `arith_wrote_nan` accepts a pipeline update (a list) rather
  than raising, which had turned `findAndModify` with `update: []` into an
  InternalError. Caught by the mongod differential gate.
