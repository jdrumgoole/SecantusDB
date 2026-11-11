### A killed `bulkWrite` test no longer races the machine's speed

`tests/mongod_live/test_interrupts.py::test_a_killed_bulk_write_fails_the_command`
failed on `main` (run 43115796507) on the reference server's own answer:
mongod 8.2.12 had applied none of the fifty updates when the kill landed,
so the test's `0 < applied < 50` precondition failed before any comparison
with the Rust server.

#### Fixed

- The test killed the `bulkWrite` at a fixed 0.3 s, which assumed the first
  update had committed by then. That holds only while the runner is as fast
  as when the test sized its slow documents. It now kills once the first
  update has committed. Measured against mongod 8.2.11 with each document
  ten times slower than sized: before, none applied in 16 of 16 runs; after,
  the kill lands inside the window in 20 of 20 (and 20 of 20 at the sized
  cost).
