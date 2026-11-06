### The RP-303 regression test re-aims a cancel that left too late

#### Fixed

- `tests/backlog/test_rp_named_parse_cancel.py` aims a cancel at the middle
  of the window between a named and an unnamed Parse. On a slow CI runner
  (PR #2397) the target Parse answered 0.27 s before that cancel left, so
  the trial tested nothing and the test failed on it. Such a trial is now
  aimed again, up to three times, at the middle of the window the Parse
  actually showed; a cancel that leaves in time and is lost still fails.
- RP-380 (`pgbench -i -s 1` over 120 s in CI) records its second sighting.
