### The standalone binary links the WiredTiger its build just refreshed

`./inv rust-binary-build` rebuilds its own WiredTiger whenever a
`cmake/patch_wt_*.py` patcher changes, but then linked whichever WiredTiger
build directory sorted first, and the wheel's (`build/cp312-*/wt-build`,
re-patched only by `rust-server-build`) sorts ahead of it. A checkout that had
built the wheel once could therefore put an unpatched WiredTiger into
`secantusd-rs` after a patcher landed. The binary tasks now link the directory
they refreshed.

#### Fixed

- `rust-binary-build`, `rust-binary-test`, `rust-mdb-coverage` and
  `rust-repro` hand cargo the WiredTiger `_build_wiredtiger` returned instead
  of the first one `_find_wt_build` finds (`rust-repro` now refreshes it
  first, as the others do). An explicit `SECANTUS_WT_INCLUDE` /
  `SECANTUS_WT_LIB` still wins.
- Fixes RM-388 (tests/backlog/test_rm_found_2026_11_07b.py::test_the_binary_links_the_wiredtiger_its_build_refreshed)
