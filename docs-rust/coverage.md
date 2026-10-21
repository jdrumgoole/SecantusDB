# Test coverage

How much of the Rust MongoDB server's source its tests execute, measured with LLVM
source-based coverage over an instrumented debug build. Regenerate it with
`./inv rust-mdb-coverage` from a clean main checkout (about 45 minutes); the
numbers describe the commit below, not the current tree.

- **Commit:** `a18efa37` (2026-10-21)
- **Toolchain:** rustc 1.98.1 (48a229cea 2026-09-01); reference server db version v8.2.11
- **Platform:** Darwin arm64

## Totals

| | Lines | Regions | Functions |
| --- | --- | --- | --- |
| Test suites | 82.2% | 82.0% | 84.8% |
| Driver gauges | 37.4% | 36.9% | 41.8% |
| **Both** | 82.8% | 82.7% | 85.2% |

Over 106,317 lines in 9 crates.

## By crate (lines)

| Crate | Lines | Suites | Gauges | Both | Added by the gauges |
| --- | --- | --- | --- | --- | --- |
| secantus-commands | 51,071 | 83.4% | 38.6% | 84.0% | +303 |
| secantus-core | 31,321 | 78.7% | 26.2% | 79.2% | +145 |
| secantus-storage | 18,550 | 83.8% | 49.3% | 84.7% | +167 |
| secantus-server | 1,863 | 86.6% | 47.8% | 87.0% | +6 |
| secantus-storage-adapter | 1,217 | 80.9% | 71.3% | 85.1% | +52 |
| secantus-wire | 923 | 90.4% | 26.7% | 90.4% | +0 |
| secantusdb | 477 | 64.4% | 27.7% | 66.7% | +11 |
| secantus-wt | 458 | 85.8% | 72.7% | 85.8% | +0 |
| secantus-auth | 437 | 95.4% | 56.1% | 95.4% | +0 |

## Where the gauges reach code the suites do not

| File | Lines added | Suites | Both |
| --- | --- | --- | --- |
| `secantus-storage/src/changestreams.rs` | +94 | 76.2% | 90.6% |
| `secantus-storage/src/lib.rs` | +71 | 84.6% | 85.1% |
| `secantus-core/src/group.rs` | +61 | 73.6% | 77.2% |
| `secantus-commands/src/lib.rs` | +59 | 82.9% | 85.4% |
| `secantus-storage-adapter/src/lib.rs` | +52 | 80.9% | 85.1% |
| `secantus-commands/src/findandmodify.rs` | +40 | 71.8% | 78.9% |
| `secantus-commands/src/changestream.rs` | +37 | 74.6% | 80.3% |
| `secantus-core/src/diff.rs` | +29 | 88.0% | 91.1% |
| `secantus-commands/src/failpoints.rs` | +26 | 86.1% | 95.1% |
| `secantus-core/src/query.rs` | +20 | 73.7% | 74.6% |
| `secantus-core/src/expressions.rs` | +18 | 67.1% | 67.4% |
| `secantus-commands/src/mongod_codes.rs` | +17 | 5.9% | 9.6% |
| `secantus-commands/src/argtypes.rs` | +16 | 87.9% | 88.5% |
| `secantus-commands/src/aggregate.rs` | +14 | 81.1% | 81.4% |
| `secantusdb/src/main.rs` | +11 | 53.8% | 58.5% |

## Most lines not executed

| File | Lines not executed | Coverage |
| --- | --- | --- |
| `secantus-storage/src/lib.rs` | 2,476 | 85.1% |
| `secantus-core/src/expressions.rs` | 2,196 | 67.4% |
| `secantus-commands/src/aggregate.rs` | 697 | 81.4% |
| `secantus-core/src/query.rs` | 568 | 74.6% |
| `secantus-commands/src/admin.rs` | 533 | 86.3% |
| `secantus-commands/src/storage.rs` | 508 | 13.9% |
| `secantus-core/src/decimal.rs` | 463 | 78.8% |
| `secantus-commands/src/mongod_codes.rs` | 412 | 9.6% |
| `secantus-core/src/group.rs` | 394 | 77.2% |
| `secantus-core/src/timelib/scan.rs` | 393 | 57.4% |
| `secantus-commands/src/validation_errors.rs` | 376 | 53.4% |
| `secantus-core/src/update.rs` | 370 | 82.9% |
| `secantus-commands/src/argtypes.rs` | 336 | 88.5% |
| `secantus-commands/src/lib.rs` | 336 | 85.4% |
| `secantus-core/src/projection.rs` | 276 | 66.0% |

## What ran

Exit status and summary line of each run. A gauge's pass count here is not its
published rate: this is an instrumented debug build.

| Run | Exit | Summary |
| --- | --- | --- |
| cargo test (workspace) | 0 | 22 test binaries: 1143 passed, 0 failed, 1 ignored |
| cargo test (secantus-storage) | 0 | 39 test binaries: 343 passed, 0 failed, 0 ignored |
| cargo test (secantus-storage-adapter) | 0 | 36 test binaries: 451 passed, 0 failed, 0 ignored |
| cargo test (secantusdb) | 0 | 5 test binaries: 18 passed, 0 failed, 0 ignored |
| cargo test (secantus-wt) | 0 | 3 test binaries: 6 passed, 0 failed, 0 ignored |
| backlog verify --regressions | 0 | === 27 OPEN, 1 SKIPPED, 39 UNCHECKED |
| tests/mongod_live | 0 | 75 passed in 11.22s |
| binary smoke + crash durability | 0 | 9 passed in 6.28s |
| gauge go | 0 | go_validation: 599 passed, 11 failed, 49 skipped (per-package breakdown in the markdown report) |
| gauge java | 0 | java_validation: 495 passed, 1 failed, 404 skipped (per-module breakdown in the markdown report) |
| gauge kotlin | 0 | kotlin_validation: 340 passed, 0 failed, 198 skipped (per-module breakdown in the markdown report) |
| gauge node | 0 | node_validation: 357 passed, 1 failed, 6 pending (364 total) |
| gauge ruby | 0 | ruby_validation: 293 passed, 1 failed, 24 pending (318 total) in 26.0s |
| gauge rust | 0 | rust_validation: wrote .validation/rust-raw-rust-server.json (101 passed, 0 failed, 0 ignored) |
| gauge php_lib | 0 | Tests: 3130, Assertions: 6226, Errors: 1, Failures: 1, Warnings: 1, Deprecations: 1, Skipped: 27. |
| gauge php_ext | 0 | Tests passed    :   680 ( 95.1%) (100.0%) |
| gauge c | 0 | c_validation: merged 838 results from both passes into .validation/c-raw-rust-server.json |
| gauge cxx | 0 | (no count in its log) |
| gauge dotnet | 0 | Passed!  - Failed:     0, Passed:   228, Skipped:     0, Total:   228, Duration: 2 m 5 s - MongoDB.Driver.Tests.dll (net10.0) |

## What is not counted

- The two pymongo gauges, and the Python test files that run the server through the
  embedded `_secantus_server` extension: instrumenting it needs the CMake wheel build.
- Branch coverage, which needs a nightly compiler.
- `#[cfg(test)]` modules inside `src/` count as source, which raises the figures
  slightly.
