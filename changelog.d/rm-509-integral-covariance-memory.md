### Rust MongoDB server: `$integral` and the covariances count their memory in a window

`$setWindowFields` now counts what `$integral`, `$covariancePop` and
`$covarianceSamp` hold the way mongod 8.2 does. `explain` reports their
`maxFunctionMemoryUsageBytes`, and the stage refuses under the same
`internalDocumentSourceSetWindowFieldsMaxMemoryBytes` budgets mongod does,
where before it counted nothing for them and ran under budgets mongod refuses.

#### Fixed

- A removable `$integral` counts its own size and each `[sortBy value, input]`
  point in its window twice: once in its deque, once in the executor's
  queue. A removable covariance counts its size and each `[x, y]` once.
  Both count one value more for the moment the window slides.
- With no lower end, `$integral` counts its accumulator and every point it
  was given, since mongod's function never lets one go. A covariance counts
  its accumulator alone. Either counts nothing while its window is still
  empty.
- The sizes follow the mongod build: `$integral`'s differ between libc++
  (macOS arm64: 272 and 368) and libstdc++ (Linux: 304 and 400) by the
  `std::deque` it keeps. A covariance's are 472 and 568 on both.

Fixes RM-509 (tests/backlog/test_rm_found_2026_11_10_rm404.py::test_integral_and_covariance_count_their_memory)
