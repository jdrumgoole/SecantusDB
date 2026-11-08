### Spill sizes are compared with each platform's own mongod

mongod 8.2 writes spill files of different sizes on arm64 and x86_64. The same in-order sort stores 130,545 bytes on Apple silicon and 131,252 on CI's x86_64 Linux. Each platform's mongod is the reference. RM-325's regression test therefore compares every case on arm64. On x86_64 it leaves out the one case that still differs there, a sort after `$project` (122,288 bytes against mongod's 122,253), which is filed as RM-402 and checked on x86_64 by CI. RM-397's `$group` gap exists only on arm64, so its gap test now runs there only.

#### Changed

- `tests/backlog/test_rm_found_2026_10_26.py`: RM-325's regression test compares by platform, RM-402 has a new x86_64-only gap test, and RM-397's gap test runs on arm64 only. Filed RM-402.
