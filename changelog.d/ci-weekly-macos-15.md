### The weekly macOS build runs again, on macos-15

GitHub retired the `macos-14` runner image, so the weekly cron's macOS
`storage-engine` cell stopped starting ("The configuration
'macos-14-arm64-us-default' is not supported"): macOS went untested from
2026-11-09. The cell now runs on `macos-15`, the runner the release binaries
already build on.

#### Changed

- The `storage-engine` job's weekly and dispatch matrix uses `macos-15`.

#### Fixed

- `tests/test_rust_binary_pitr.py` and `tests/test_crash_recovery.py` read
  their server's stderr for its whole life instead of leaving the pipe
  unread until the end. A pipe nobody reads fills (64 KiB on Linux and
  macOS, about 4 KiB on Windows) and blocks every server thread that writes
  to it -- the deadlock that failed `test_rust_binary_smoke.py` on Windows.
