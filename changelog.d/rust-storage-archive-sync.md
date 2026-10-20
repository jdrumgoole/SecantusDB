### Rust storage: PITR segments and backup archives reach the disk

#### Fixed

- A point-in-time-recovery oplog segment is synced, and so is the directory
  holding its name, before the rows it holds are deleted from WiredTiger.
  Before, a power loss could leave a permanent gap in the recovery chain:
  the delete became durable at the next checkpoint while the segment might
  still be only in the page cache.
- `create_archive` writes to a temporary name, syncs it, renames it into
  place and syncs the directory. A failure part-way no longer leaves a
  truncated archive under the final name.

Fixes RM-190. Its Verify line is `none` (it needs a power loss);
`tests/test_rust_binary_pitr.py` covers the paths. The change is in the
shared `secantus-storage` crate, which the Rust PostgreSQL server also links.
