### READ COMMITTED reads its own rows without replaying, and avg streams

A READ COMMITTED transaction block on the Rust PostgreSQL server that reads
a table it has written, while other sessions keep committing to that same
table, no longer replays its whole write set onto a new snapshot for every
statement. The read runs in a fresh snapshot with the block's own rows laid
over it, so it sees every commit since and its own uncommitted writes, as
PostgreSQL does: 800 insert-then-aggregate pairs beside a committing writer
went from 24.7 s to 8.6 s on a debug build.

`avg` over any numeric or floating-point column, and `sum` over a float
column, with no GROUP BY, are now computed a chunk at a time in bounded
memory, with answers bit-identical to the one-pass result: 300,000 rows of
2 KB went from about 1.75 GB of server memory growth to about 64 MB.

#### Added

- `secantus-storage`: `Storage::block_overlay`, `TableOverlay`,
  `with_read_overlay` and `Storage::oplog_async`. `find_matching_with`,
  `scan_batch_after`, `scan_matching_batches`, `count_matching` and
  `find_by_id` honour an installed overlay; nothing installs one on the
  MongoDB server.

#### Changed

- `secantus-pgserver`: a READ COMMITTED SELECT of the narrowed shapes over
  tables the block wrote reads apart under the block's overlay
  (`rc_overlays`) instead of moving the block. Ungrouped `avg` and float
  `sum` stream (`ungrouped_in_bounded_memory`): a float total is carried
  from chunk to chunk, an exact average keeps its sum and count.
