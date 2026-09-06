### A read-path sweep: queries that returned the wrong documents

A 385-case differential sweep of `find` filters, projections and sorts against
mongod 8.2.11 — comparing full **result sets**, not counts, so a missing
document is visible rather than a wrong tally. It found 27 divergences on the
Python server and 20 on the Rust one. This release fixes thirteen of them, in
four families, and every one was a wrong answer rather than a wrong message.

A **NaN range bound matched nothing**. mongod's comparison order treats NaN as
equal to NaN — which is why `find({x: NaN})` works — so an inclusive bound
matches it: `{$gte: NaN}` and `{$lte: NaN}` return the NaN document while
`{$gt: NaN}` and `{$lt: NaN}` return nothing. IEEE says every NaN comparison is
false, so both servers returned nothing for all four.

**Decimal128 could not be compared with an infinity.** The numeric bridge bailed
out whenever either operand was non-finite, and only NaN needed excluding —
`Decimal` orders ±Infinity perfectly well. The bail-out left `float > Decimal128`
to raise `TypeError`, which the caller swallows into a silent no-match, so
`{x: {$gt: Decimal128("5")}}` skipped a document holding `Infinity` and
`{x: {$lt: Infinity}}` skipped one holding `Decimal128("5")`. `$all` had the
same shape from a different cause: it compared elements with a bare `==`, so
`{$all: [5]}` missed a stored `Decimal128("5")` that `$eq: 5` matched.

**NaN had no place in the sort order.** The comparator answered "not less" in
both directions, so a sort left a NaN wherever the algorithm happened to put it —
between `5.5` and `Infinity` in a measured case. mongod ranks it below every
other number, `-Infinity` included. BinData was mis-ordered too: mongod compares
by length first and then by bytes, so `b"\x02"` sorts before `b"\x01\x02"`.

**`$type` accepted eight aliases it could never match.** On the Rust server
`javascript`, `minKey`, `maxKey`, `timestamp`, `undefined`, `symbol`,
`dbPointer` and `javascriptWithScope` all validated as arguments and then
matched no document, because the type table had no arm for them. Argument
validation passing is what made the gap invisible.

#### Fixed

- `secantus.query` / `secantus-core`: `$gte` / `$lte` with a NaN bound match the
  NaN document; `$gt` / `$lt` still match nothing.
- `secantus.query`: the numeric bridge admits ±Infinity, so Decimal128 compares
  against them under every range operator instead of silently matching nothing.
- `secantus.query`: `$all` compares elements with the same numeric-bridging
  equality `$eq` uses.
- `secantus.ordering`: NaN sorts below every other number and ties with another
  NaN; BinData sorts by length, then bytes.
- `secantus-core`: `$type` matches the eight BSON types its alias table already
  accepted.
