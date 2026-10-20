### Rust storage: per-collection write locks are let go

#### Fixed

- The per-collection write lock map kept an entry for every namespace a
  server had ever seen. Once the map passes its threshold (1,024 entries,
  doubling past what is still held), a new namespace now prunes the locks
  no one holds. A lock is cloned only under the map's own mutex, so pruning
  an unheld one cannot split a namespace's writers across two mutexes.

Fixes RM-194 (Verify: none, since it is unbounded growth over a server's
lifetime). `crates/secantus-storage/tests/coll_locks.rs` creates and drops
5,000 collections and checks the map stays near its threshold. The change
is in the shared `secantus-storage` crate.
