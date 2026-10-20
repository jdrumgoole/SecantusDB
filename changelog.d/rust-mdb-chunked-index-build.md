### Rust MongoDB server: an index build no longer hangs under a small cache

#### Fixed

- `createIndexes` outside a transaction now writes its entries in bounded
  transactions: at most 1,000 entries or 512 KB each. The catalog row is
  committed last, so a reader never routes through a half-built index.
  Before, the whole build was one transaction. Under a cache too small for
  it, WiredTiger rolled it back, and the write-conflict loop re-ran it
  forever. At `--cache-size 8M`, 5,000 300-byte documents never finished;
  now they finish in 0.1 s. At 32 MB, 300,000 such documents finish in
  4 s; that build was still retrying at 120 s.
- A build first removes any entries a build that died part-way left under
  its name.

Fixes RM-187's hang
(`tests/backlog/test_rm_found_2026_10_20.py::test_index_build_completes_under_a_small_cache`,
`crates/secantus-storage/tests/index_build_chunk.rs`). Still open under
RM-187: the build holds writes to its collection off until it finishes,
where mongod's lets them continue. The change is in the shared
`secantus-storage` crate, which the Rust PostgreSQL server also links.
