### A line-coverage report for the Rust MongoDB server

The Rust MongoDB server's documentation now has a test-coverage page,
measured with LLVM source-based coverage over an instrumented build. It
reports the code the project's own test suites execute, the code the
eleven driver-conformance gauges execute, and the two together, for each
crate, with the files where the gauges reach code the suites do not and
the files with the most lines never executed.

#### Added

- `scripts/rust_mdb_coverage.py` and `./inv rust-mdb-coverage`: build every
  crate of `secantusd-rs` with `-C instrument-coverage`, run the cargo
  tests, the Python suites CI runs against the binary and the daemon driver
  gauges, and write `docs-rust/coverage.md`.
- `docs-rust/coverage.md`: the first measurement.
