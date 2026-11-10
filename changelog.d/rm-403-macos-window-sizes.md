### Window and group memory counted at the sizes of the platform's own mongod

A window function's memory and a `$group` accumulator's own size depend on how mongod was built. Linux mongod, built against libstdc++, counts a classic `$sum` at 176 bytes. macOS mongod, built against libc++, counts it at 184. Each platform's mongod is the reference, and the Rust MongoDB server now counts the sizes of the build it runs on. On Apple silicon these are macOS mongod's: a `$sum` or `$count` accumulator of 184 bytes, removable window functions 8 bytes larger for `$sum` and `$avg`, 16 larger for `$stdDevPop` and 32 smaller for `$push`, and rank functions 32 smaller. Everywhere else they stay Linux's. So a `[-5, 5]` `$sum` window runs under 1230 bytes on a Mac and 1222 on Linux, as on each platform's mongod, and a spilled `$group` of `$sum` and `$count` reports mongod's `groupSpilledBytes`. `explain` now also counts what mongod's removable windows hold for a moment: they take in the value entering the window before they let go of the one leaving it, so `maxFunctionMemoryUsageBytes` and `maxTotalMemoryUsageBytes` count both.

#### Fixed

- `secantus-core` (`memlimit`, `group`, `windowfields`): a `Layout` of the function and accumulator sizes that follow from mongod's build, Linux's (libstdc++) and macOS arm64's (libc++), chosen by the target, as `explain/entry_size.rs` chooses the plan-cache layout. Measured on mongod 8.2.1 and 8.2.11, which agree.
- `secantus-core` (`windowfields`): a removable window function's maxima include the values leaving its window while the entering ones go in, as `WindowFunctionExecRemovable{Document,Range}::update` (r8.2.1) adds before it removes. The budget check still counts the window alone, as mongod's does.
- Fixes RM-403 (tests/backlog/test_rm_found_2026_10_26.py::test_macos_window_function_sizes).
- Fixes RM-397 (tests/backlog/test_rm_found_2026_10_26.py::test_group_sum_spilled_bytes), the `$group` face of the same sizes.
- Fixes RM-507, filed and fixed here (tests/backlog/test_rm_found_2026_10_26.py::test_removable_window_peak_holds_the_leaving_value).
- `test_window_budget_residuals` now compares every window on every platform.
