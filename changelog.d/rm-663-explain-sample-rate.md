### `explain` refuses a `$sampleRate` the run refuses

`explain` of an aggregate whose `$match` holds `{$sampleRate: 2}` or
`{$sampleRate: "x"}` now answers BadValue, as mongod 8.2.11 does, instead
of listing the stage.

#### Fixed

- The rate is checked when `explain` parses the pipeline, under `$and` /
  `$or` / `$nor` too, with the run's own messages.

Fixes RM-663 (tests/backlog/test_rm_found_2026_11_14_rm538.py::test_explain_refuses_a_sample_rate_the_run_refuses)
