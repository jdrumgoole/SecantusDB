### `$elemMatch` traversed an array twice

mongod applies implicit array traversal **once per path step**, and
`$elemMatch` spends that step choosing the element — so inside it the element is
a terminal value and nothing descends into it again. Both servers got that
wrong, in opposite directions.

The **operator** form matched *through* an element that was itself an array, so
`{$elemMatch: {$gt: 1}}` returned documents holding `[[5]]` and `[1, [2, [3]]]`;
mongod returns neither, because an array is not greater than 1. The **criteria**
form had the mirror-image gap: it considered only document elements, so
`{$elemMatch: {}}` — a criteria that imposes no field requirement — missed every
document whose array holds an array element, which mongod returns.

The fix is a `descend` flag threaded through the operator dispatch, off for the
element match, so one rule covers every operator rather than each one growing
its own special case. Three operators needed reaching individually because they
recurse or iterate the array themselves, and each was a separate miss in the
first version of the fix — `$in` has its own candidate path, `$not` re-enters
the field matcher, and `$all` walks the array. Two were caught by writing the
test and one by probing before believing the fix was complete.

Two neighbouring divergences from the same 8.2.11 sweep are **not** fixed here
and are filed with their measured rules, because they live in path resolution
rather than in matching: a dotted POSITIONAL path (`x.0`) descends one level too
far, and a dotted sort key does not descend at all.

#### Fixed

- `secantus.query` / `secantus-core`: `$elemMatch`'s operator form treats the
  element as terminal, so it no longer matches through a nested array. Applies
  to every operator it can carry — comparison, `$in` / `$nin`, `$type`,
  `$regex`, the `$bits*` family — via one flag rather than per-operator logic.
- `secantus.query` / `secantus-core`: `$elemMatch`'s criteria form reaches array
  elements when the criteria names no field, so `{$elemMatch: {}}` returns what
  mongod returns. A non-empty criteria still requires a document element.
- `secantus.query` / `secantus-core`: `$not`, `$in` / `$nin` and `$all` carry
  the flag through their own recursion, so `{$elemMatch: {$not: {$gt: 1}}}` no
  longer inverts a descended match and `{$elemMatch: {$all: [5]}}` no longer
  matches a nested array. `$size` and a nested `$elemMatch` were already right
  and are pinned so they stay that way.
