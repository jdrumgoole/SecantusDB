### Rust MongoDB server: a `$group`'s N accumulators count the values they keep, as mongod's do

A classic `$group` with `$firstN`, `$lastN`, `$maxN`, `$minN`, `$top`, `$bottom`, `$topN` or `$bottomN` now counts the memory of the values it keeps, so it spills at the same points as mongod 8.2. Before, it counted only the accumulator's own size: over 300 groups of a 1000-byte string with a 5000-byte budget, a `$firstN` spilled 13 times where mongod spills 60, and a `$topN` 38 times where mongod spills 75. In the classic engine the accumulator now keeps only what mongod's keeps: at most `n` values, dropping each value a better one replaces. It counts each kept value at mongod's size and stops counting it once it is dropped.

A `$median` / `$percentile` now counts its t-digest's buffer as mongod does, and the fixed sizes of the N accumulators follow the platform's mongod build: libstdc++ on Linux, libc++ on macOS arm64.

#### Fixed

- `secantus-core` (`group.rs`): the classic `$firstN` / `$lastN` / `$maxN` / `$minN` keep and count what `AccumulatorFirstLastN` / `AccumulatorMinMaxN` keep, at each value's `getApproximateSize`. `$top` / `$bottom` (N) count each kept item's sort key, output and `sizeof(KeyOutPair)`, as `AccumulatorTopBottomN` does. The kept values finalize to the same answer as every value. SBE's accumulators are unchanged.
- `secantus-core` (`group.rs`): `$median` / `$percentile` count `sizeof` the accumulator and its `TDigest` (256) plus the buffer's capacity in doubles, which doubles from one and keeps the capacity it reaches when a flush empties it. Infinities are not buffered.
- `secantus-core` (`memlimit.rs`): the N accumulators' own sizes are per platform: `$firstN` / `$lastN` 200 / 168, `$maxN` / `$minN` 168 / 152, `$top` / `$bottom` (N) 544 / 472 (Linux 8.2.12 / macOS 8.2.11). The old `$top` 608 and `$topN` 640 each included kept items.
- Fixes RM-502 (`tests/backlog/test_rm_found_2026_10_26.py::test_group_held_value_spills`, now also covering `$lastN`, `$bottom`, `$topN` over many documents, `$median` and a two-field `_id`), with unit tests in `group::held_value_memory_tests`.
- RM-613's gap test now compares the spill file's size (`groupSpilledDataStorageSize`: mongod 5561, this server 2948), because the `groupSpilledBytes` it compared before is the memory estimate this fix made match.
