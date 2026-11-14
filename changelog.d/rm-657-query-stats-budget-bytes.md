### Rust MongoDB server: the query stats budget is 1% of the memory in bytes

On Linux, `metrics.queryStats.maxSizeBytes` with `internalQueryStatsCacheSize`
at its default (`1%`) was a few hundred bytes short of mongod 8.2.11's,
because the server took the memory as whole MiB before taking 1% of it.
It now keeps the memory in bytes (`MemTotal` kB times 1024, `hw.memsize`
on macOS), as mongod's `ProcessInfo` does, and sizes the store and its
partitions from that. `hostInfo`'s `memSizeMB` is unchanged.

#### Fixed

- Fixes RM-657 (tests/backlog/test_rm_found_2026_11_14_query_stats_size.py::test_the_query_stats_budget_is_a_share_of_the_memory_in_bytes)
