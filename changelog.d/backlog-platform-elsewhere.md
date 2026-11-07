### The backlog can hold a gap that exists on one platform only

mongod's own answer sometimes differs between its arm64 and x86_64 builds. Sort spill sizes are one example: the same in-order sort stores 130,545 bytes on Apple silicon and 131,252 on x86_64 Linux. Each platform's mongod is the reference, so a gap can be real on one platform and absent on the other. A gap test now calls `only_on("x86_64", why=...)` from `tests/backlog/conftest.py` before anything else. On other platforms it skips itself as `platform: ...`, and `tools/backlog.py verify` reports that as `ELSEWHERE`. That status is neither a pass nor a failure, and a regression test skipped the same way is not `REGRESSED`.

#### Added

- `tests/backlog/conftest.py`: `PLATFORM` and `only_on(*platforms, why=...)`.
- `tools/backlog.py verify`: the `ELSEWHERE` status, from the pure helpers `elsewhere()` and `regressions_failed()`.
- `tests/test_backlog_platform.py`: pins both helpers and the skip prefix.
