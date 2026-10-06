### Groundwork for READ COMMITTED reads without a write-set replay

The storage layer can now describe a transaction's own writes to one table as
a per-row map -- its current version of every row it inserted or updated, and
a tombstone for every row it deleted -- and lay that map over a fresh
snapshot when streaming a table. This is the first step toward letting a READ
COMMITTED block on the Rust PostgreSQL server see other sessions' newer
commits without replaying everything it has written. Nothing uses it yet: the
MongoDB server never installs an overlay, and the PostgreSQL server does not
install one until every read path a statement can reach honours it.

#### Added

- `secantus-storage`: `Storage::block_overlay(handle, db, coll)` (a
  `TableOverlay` keyed by `_id` key, or `None` when the write set holds a
  command on the table or the oplog is asynchronous), `with_read_overlay`, and
  `Storage::oplog_async`. `Storage::scan_matching_batches` merges an installed
  overlay in RecordId order, re-applying the filter to the block's rows.
