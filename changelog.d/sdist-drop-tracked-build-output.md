### The source distribution is back under PyPI's file-size limit

`0.7.0b0` reached PyPI as wheels only. Its source distribution came out at
573 MB, against 14 MB for `0.6.0b17`, and PyPI refuses any file over 100 MB. A
cargo build directory, `crates/secantus-storage/target-dev/`, had been
committed to the repository: 6,875 files and 1.77 GB, all of which the sdist
picked up.

#### Fixed

- The build directory is no longer tracked, and every cargo target directory
  under `crates/` is now ignored by git and excluded from the sdist.
- A test fails if a cargo target directory, or any file over 8 MB, is tracked.
