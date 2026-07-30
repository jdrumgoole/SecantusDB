### The Python server's change streams get the oplog visibility point too

The Rust server's oplog visibility fix has a twin on the Python server.
Since the per-collection lock split, Python writers on different
collections mint their oplog sequence numbers and commit their WiredTiger
transactions independently — so a writer could commit a *later* sequence
while an earlier one was still inside an open transaction, and a change
stream polling in that window advanced its resume position (the
`scan_high` skip bound) past the hole. When the in-flight transaction
committed, its event sat behind the stream's position: dropped live and
unreachable on resume. Multi-document transactions were already protected
by commit-time minting, but the flush between mint and WiredTiger commit
had the same narrow window.

The fix is the same `all_durable`-style design: an in-flight mint window
pins the visible tail at its floor, registered when sequences are minted
and released when the owning transaction commits or rolls back (batch
transactions, the user-transaction commit flush, and bare autocommit
emits each resolve at their own point). Every reader is bounded by it —
the tailable-getMore wake predicate, change-stream open positions,
`read_oplog` and its `scan_high`, the PITR archive head, and
`startAtOperationTime` (which now waits briefly for the window to drain
past its answer instead of finalising a position an in-flight event could
land behind). A rolled-back mint leaves a permanent, tolerated hole and
can never stall the stream.

#### Fixed

- Python server: change streams no longer lose events when writers on
  different collections commit out of oplog-mint order (live and on
  resume); `startAtOperationTime` can no longer skip an event minted
  inside a still-open transaction. Five WT-level pinning tests
  (`tests/test_oplog_visibility.py`) mirror the Rust suite.
