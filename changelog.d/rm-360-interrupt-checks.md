### The Rust MongoDB server checks for interrupt where mongod does, and counts it

mongod checks an operation for `killOp` and `maxTimeMS` at fixed points:
once at dispatch, at every lock it is granted, in its planner, as a cursor
left open is checked back in, and, while a plan runs, only at a yield, every
10 ms (`internalQueryExecYieldPeriodMS`). Its `Slow query` line counts these
checks as `numInterruptChecks`. The Rust MongoDB server used to poll once
every 64 documents. It now checks at mongod's points, yields at mongod's
interval, and counts the checks. So `killOp` and `maxTimeMS` reach a running
read where they reach one on mongod, and a line's `numYields` is a real count.

A `find`, `count` or `distinct` with a single candidate plan, a `getMore` of
a `find`, `listIndexes`, `listCollections`, `collStats`, `ping` and
`buildInfo` print mongod 8.2's count. Other lines print none until their
checks are where mongod's are.

#### Fixed

- `secantus-core` `deadline`: `check_for_interrupt` is mongod's counted
  `checkForInterruptNoAssert`. The per-document poll (`check`) is the plan
  executor's yield point: every `internalQueryExecYieldPeriodMS` (10 ms) it
  yields and checks, and counts the yield. The check inside an expression is
  mongod's `ExpressionContext` checker, which checks every 128th call.
- `secantus-commands`:
  - interrupt checks at dispatch, at each lock grant (`LockManager::acquire`),
    in planning (one per index scan of a single-candidate plan, then one when
    planning ends, or the express executor's one), at a `getMore`'s pinned
    cursor, and at a cursor's check-in, which kills the cursor when the check
    fails;
  - a yield counts the re-take of the statement's locks;
  - `numYields` on the line and the profile entry is the operation's own;
  - `numInterruptChecks` is printed where the count is mongod's;
  - `internalQueryExecYieldPeriodMS` is a `getParameter` / `setParameter`
    parameter.
- Narrows RM-360 to the plans with several candidates, pipelines, writes and
  DDL (tests/mongod_live/test_interrupt_checks.py::test_a_line_counts_mongods_interrupt_checks,
  tests/backlog/test_rm_355_364.py::test_a_line_counts_its_interrupt_checks).
