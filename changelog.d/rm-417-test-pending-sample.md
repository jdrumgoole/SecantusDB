### RM-417's period test allows for the old period's last tick

`tests/backlog/test_rm_219_diag_query_settings.py::test_the_period_and_the_switch_take_effect` sets the FTDC period to 60 seconds and expects the sample to hold. The collector is part-way through its wait for the next one-second tick when the period changes, and it samples once more when that wait ends. On a loaded CI runner that sample came after the test's fixed 1.2-second settle, and mongod read as not holding (PG snapshot #2664). The test now looks for a quiet 1.2 seconds in up to three windows. mongod held in 4 of 4 runs, and a stand-in that kept sampling every second still fails. RM-421's work found the other cause: both servers sample on whole multiples of the period, so a window that crosses a whole minute sees a sample. The test now also starts at least 5 seconds clear of the next minute.

#### Fixed

- `test_the_period_and_the_switch_take_effect` no longer fails on the old period's last tick or on a whole-minute sample.
