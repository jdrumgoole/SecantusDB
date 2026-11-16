### The pytest stall dump now shows what the workers were doing

When the test suite stops reporting, the xdist controller prints "pytest session stalled" and the thread stacks of the process that noticed. That process is the controller, and its threads are always the same idle receive loops: the hung test lives in a worker, or in a server a worker started, so the dump never named it. One CI stall (a backlog-verifier run, 2026-10-21) went undiagnosed for that reason.

The stall dump now also signals every worker, which writes every thread's Python stack to a file, and prints those stacks. It lists the processes the workers started (the servers) with their commands, and adds a native stack of each, from `sample` on macOS or `gdb` where it is installed.

#### Added

- `tests/conftest.py`: workers register a `faulthandler` `SIGUSR1` dump; the controller's stall report collects it with a listing and native sample of the worker-started processes. `tests/test_crash_stall_watchdog.py::test_stall_fails_fast_with_stacks` pins that the sleeping test's frame appears in the dump. Fixes RM-254.
