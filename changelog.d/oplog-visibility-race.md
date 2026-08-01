### Change streams no longer skip an event that commits mid-lookup

Resuming a change stream from a point in time could permanently miss an event.
Mapping a `startAtOperationTime` to a position scans the committed oplog and
then checks that nothing is still in flight below the answer — but it read
those two things in the wrong order. A write that committed between the scan
and the check produced a stale answer naming the position *above* it, while
the check had already advanced to cover that position, so the answer was
accepted and the event was never delivered.

The two reads are now ordered so the in-flight check is sampled first, which
is conservative in the safe direction: the visible position only ever moves
forward, so an earlier reading can only make the check stricter, never let an
unresolved write slip past.

The window was narrow enough to surface only as an intermittent CI failure on
Windows, where the coarser scheduling quantum happened to land inside it. It
is reproducible on demand once the interleaving is forced, and the regression
test does exactly that rather than racing for it. Both the Python and the Rust
storage engines carried the same ordering and both are fixed.

#### Fixed

- `startAtOperationTime` could resolve to a position past an in-flight write
  whose entry qualified, permanently skipping that event once it committed.
