### Rust PG server: a cancel while a huge `IN` list is analysed is honoured at once under load

PostgreSQL 15.19 answers a cancel about 0.015 s after it is sent, at its
next `CHECK_FOR_INTERRUPTS`. While this server analysed a 200,000-item
`IN` list, the operator type check walked every item through RP-530's row
check with no cancellation point: about 70 ms in a debug build, and about
half a second under load, so RP-202's regression test failed in lanes and
on CI whenever the machine was busy. A trace found the stretch.

#### Fixed

- The item loops in `optype::check_aexpr` (RP-530's row check and the enum
  label check) are cancellation points every 1,024 items, as the typing
  loop after them already was. Under a load average of 11 to 16 on a
  12-core Mac, RP-202's test at `[0.7]` failed 10 runs of 10 (0.37 to
  0.53 s late) before the change and passed 10 of 10 after it, with
  cancels at most 0.045 s late over 15 further runs of each shape.

#### Added

- `SECANTUS_PG_CANCEL_TRACE=<file>`: the server appends a timestamped line
  for each cancel request, each cancellation point that takes a cancel or
  follows a stretch of 20 ms or more without one (with the check before
  it), the interruptible parse thread, each message cycle and each 57014.
  Off without the variable. RP-002's and RP-202's timing tests turn it on
  and print the statement's window of it when they miss their budget.
- `tools/backlog.py check-ids` counts an ID as new when the branch added it
  since leaving `main`, not when `main`'s tip lacks it. An ID that `main`
  gained after the fork passed as one the branch had inherited: a pg-batch
  lane wrote RP-747 while `main` held another RP-747. The lane's item is
  renumbered RP-767.

Fixes RP-748 (tests/backlog/test_rp202_cancel.py::test_a_cancel_while_a_huge_in_list_is_planned_over_a_table).
