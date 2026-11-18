### A command ingress admission refused names `<db>.$cmd` on its line

The Rust MongoDB server's `Slow query` line for a command that never got
its ingress admission ticket (its `maxTimeMS` spent, or killed, while
queued) now names `<db>.$cmd`, as mongod 8.2.11's does, rather than the
collection it would have run on.

#### Fixed

- The line of a command refused by ingress admission control names
  `<db>.$cmd`. Fixes RM-355, in part
  (tests/mongod_live/test_ingress_admission.py::test_a_queued_commands_line_says_how_long_it_waited).
