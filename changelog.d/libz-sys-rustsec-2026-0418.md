### libz-sys moves to 1.1.31 (RUSTSEC-2026-0418)

The WiredTiger binding's `libz-sys` dependency moves from 1.1.29 to 1.1.31 in every lockfile that links it, clearing RUSTSEC-2026-0418 (a build script that leaked environment variables into the build directory) and turning the `cargo-audit` gate green again. Both Rust servers link it.

#### Security

- `libz-sys` 1.1.29 → 1.1.31 in the eight WiredTiger-linked lockfiles (issue #2267).
