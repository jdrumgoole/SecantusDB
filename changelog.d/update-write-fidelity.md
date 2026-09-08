### Update operators wrote where mongod refuses

`tools/probes/update_operators.py` compares update *errors*. Nothing compared
the **document a successful update produces**, which is where a silently wrong
write hides. A sweep of 31 updates × 17 seed value classes (527 cells) against
mongod 8.2.11 found **30 divergent** in four families; all 527 now agree.

Most of it is one shape CLAUDE.md already names — **missing conflated with
null**. `get_path(doc, path, default=None)` returns `None` for a field that is
absent *and* for one that is present and null, so three operators treated a null
field as an absence and created a value over it.

#### Fixed — data was being written

- **`$push` / `$addToSet` over a null field created an array.**
  `{$push: {v: 4}}` over `{v: null}` wrote `{v: [4]}`, destroying the null,
  where mongod raises `2 The field 'v' must be an array but is of type null`.
  An *absent* field is still created, which is the half that has to keep
  working.
- **`$bit` over a null field wrote a number.** The Python side read the current
  value as `get_path(..., default=0) or 0`, which turned every *falsy present*
  value — a null, a `-0.0`, an empty array — into the integer `0` so it passed
  the integral check. mongod refuses all three.
- **`$pull` with a scalar emptied arrays of arrays.** `{$pull: {v: 1}}` over
  `{v: [[1, 2]]}` left `{v: []}`; mongod leaves the document untouched, because
  `[1, 2]` is not `1`. The scalar criterion was routed through the query engine,
  which adds implicit array traversal — the same membership-after-nesting family
  as the positional path fix. An **operator** or **regex** criterion does
  traverse, and still does: `{$pull: {v: {$gt: 1}}}` empties that same document.

#### Fixed — an invalid update reported success

- **`$rename` through a non-document path silently no-opped.**
  `{$rename: {"v.k": "v.j"}}` over any non-document `v` is
  `28 cannot use the part (v of v.k) to traverse the element ({v: 1})` on
  mongod. A genuinely *absent* path stays a no-op, which is the distinction the
  check has to preserve.
- **`$pop` over a null field no-opped on the Python server** where mongod and
  the Rust server both raise `14 Path 'v' contains an element of non-array
  type 'null'`.
