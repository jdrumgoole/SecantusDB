### A cache that switched itself off for the length of every transaction

The Rust PostgreSQL server keeps a process-wide cache of the type catalog so
that a statement need not re-read and re-decode it. Opening a transaction block
recorded the catalog version the block had seen; the statement that opened it
then advanced that version, as every transaction-control statement does. `BEGIN`
cannot change a catalog, but the bump happened anyway, and from that moment the
recorded version and the live one disagreed — which is the exact condition under
which the cache refuses to be refilled, on the reasonable ground that a block
whose view has diverged should not publish its reads as committed truth.

The consequence was that no statement inside any transaction could ever use the
cache. Counted directly, a short block took two cache hits and fifty misses,
re-reading every catalog collection from storage each time; re-anchoring the
recorded version to the bump that just happened turns that into twenty-six hits
and eight, the remainder being first-time reads.

Worth being plain about the size of it: this recovers about three microseconds
of a seventy-microsecond gap between statements inside a block and the same
statements outside one. The catalog scan sat at the top of the sampled profile
and looked like the cost; it was the most visible symbol rather than the
dominant one, and with it gone the profile is flat and the gap is still there.
What accounts for the rest is not yet known, and the backlog now says so rather
than repeating the story that turned out to be worth three microseconds.

#### Fixed

- The type-catalog cache is usable inside a transaction block again, instead of
  being disabled from the block's first statement onward.
