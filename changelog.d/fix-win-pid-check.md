### Fix: pytest-tmp reaper's liveness check works on Windows

The abandoned-pytest-tmp reaper (`invoke clean`) probed whether a
tmp-dir's owning run was still alive with `os.kill(pid, 0)` — the POSIX
existence idiom. On Windows `os.kill` rejects signal 0 with `OSError
[WinError 87]`, a plain OSError the ProcessLookupError/PermissionError
handlers never caught, so it errored instead of detecting a dead run
(failing `test_clean_pytest_tmp` on the Windows CI lane). The liveness
check is now cross-platform: Windows queries the process handle
(`OpenProcess` + `GetExitCodeProcess`); POSIX keeps the signal-0 idiom.

#### Fixed
- `_pytest_tmp_owner_alive` liveness check is cross-platform (Windows CI
  no longer errors with WinError 87).
