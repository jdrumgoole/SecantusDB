### Storage: a write reads its collection's catalog once per generation

Every insert re-read its collection's registry row (options, UUID, whether it
exists) and every index descriptor from WiredTiger, and did so while holding
the collection's write lock. So concurrent writers to one table queued behind
reads whose answer had not changed. The PostgreSQL server also listed the
table's indexes from storage on every row it wrote, to find expression
indexes. Each is now read once per catalog generation and reused while that
generation stands.

The rule is the one the query planner's index cache already relies on. Every
write of the registry or the index catalog moves the generation. A
transaction uses a cached entry only while the generation its snapshot was
taken under is still the settled one, so a catalog change in flight or just
committed is always read afresh, and so is the transaction's own. A
collection that is not registered, or that only a transaction's uncommitted
create holds, is never cached.

#### Changed

- `secantus-storage`: `index_descs_cached`, `coll_options_cached` and a
  cached `list_indexes`, used by `insert_chunk` and `enforce_capped_bounds`.
  `UserTransactionHandle::planner_gen` records the settled planner
  generation where its snapshot is taken. This is the storage both Rust
  servers link, so the MongoDB server's inserts and `listIndexes` use it
  too.
- psycopg's `test_type_error_shadow` script (150,000 prepared UUID inserts
  from three connections) went from 9.8 s to 8.4 s in a debug build
  (PostgreSQL 15.19: 8.0 s) (RP-014, part; decided by Joe, 2026-10-20).

#### Added

- `crates/secantus-storage/tests/write_catalog_cache.rs`: a unique index
  created, dropped, created inside a transaction that commits or rolls back,
  a collection dropped and recreated, renamed, or replaced by a capped one,
  each with the cache warm. A cache that ignores the generation fails it.
