### RM-417's period test allows the sample already in flight

`tests/backlog/test_rm_219_diag_query_settings.py::test_the_period_and_the_switch_take_effect` sets the FTDC period to 60 seconds and expects the sample to hold. The collector may be part-way through a wait under the old one-second period and samples once more when that wait ends. On a loaded CI runner, that sample came after the test's fixed 1.2-second settle, and mongod read as not holding (PG snapshot #2664). The test now looks for a quiet 1.2 seconds in up to three windows. mongod held in 4 of 4 runs, and a stand-in that kept sampling every second still fails.

#### Fixed

- `test_the_period_and_the_switch_take_effect` no longer fails on a sample taken under the old period.
