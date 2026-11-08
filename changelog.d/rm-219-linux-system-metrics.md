### The diagnostic sample is compared with each OS's mongod, and Linux's systemMetrics is filed

On Linux mongod adds a `systemMetrics` collector to each diagnostic sample: the machine's CPU, memory and disk counters from `/proc`. macOS mongod has none. RM-219 was built and checked on a Mac, so on CI's Linux runner its regression test found mongod's extra collector. The test now compares the collectors each OS's mongod samples. Linux's `systemMetrics` is filed as RM-424, with a gap test that runs on Linux only. A backlog gap test can now name an operating system (`linux`, `darwin`) as well as an architecture when it says where it applies.

#### Changed

- `tests/backlog/conftest.py`: `only_on` also accepts an operating system, and `OS` names this one.
- `tests/backlog/test_rm_219_diag_query_settings.py`: RM-219's regression test leaves out `systemMetrics` on Linux, and RM-424 has a Linux-only gap test. Filed RM-424.
