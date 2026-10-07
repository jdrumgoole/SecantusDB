### Concurrent durable commits share log writes on the Rust PostgreSQL server (macOS)

On macOS the Rust PostgreSQL server syncs each commit with WiredTiger's
`method=dsync`, and stock WiredTiger gives every synced commit a log write of
its own. Commits that arrive while a log write is in flight now join the next
one. With 8 clients inserting, each into its own table, throughput went from
31.5k to 37.9k statements a second (PostgreSQL 15 on the same machine: 41.5k);
UPDATE went from 29.6k to 33.5k. One to four clients are unchanged. A commit is
still acknowledged, and still becomes visible to other sessions, only after its
log record is written.

This is the first patch to the vendored WiredTiger that changes behaviour
rather than the build. It applies to `method=dsync` commits only. The MongoDB
servers, and the PostgreSQL server on Linux, use `method=fsync` and run the
unpatched code.

#### Changed

- `cmake/patch_wt_dsync_group.py` patches `src/log/log.c` and
  `src/log/log_slot.c`: a dsync commit copies its record into the active log
  slot without closing it, and the first waiter to find every earlier slot
  written closes the slot and writes the group. It is applied by the wheel's
  CMake build, the `secantus-wiredtiger-sys` crate's bundled source and
  `./inv rust-wt-build`.
- A dsync commit no longer takes `log_sync_lock` to check for a directory sync
  that has already happened. Losing that try-lock stalled the commit for 10 ms.

#### Fixed

- An existing build directory now rebuilds WiredTiger when a
  `cmake/patch_wt_*.py` script is added or edited. The patch step's recorded
  command never changed, so it kept the library it had.
- `./inv rust-wt-build` built WiredTiger from the unpatched submodule; it now
  applies the same patches as the crate's bundled copy.
- `secantus-wiredtiger-sys` relinks when a prebuilt `libwiredtiger.a` named by
  `SECANTUS_WT_LIB` is rebuilt in place.
- `bench/pg_concurrency.py`'s `update` and `select` workloads addressed a row
  that was never seeded, so they measured statements that matched nothing.
  Both benchmarks also measured the main checkout's binary from any worktree.
