### The macOS release binary builds again

The macOS `secantusd-rs` for 0.5.3-beta.166 was not published, because its
profile-guided-optimisation (PGO) build failed. 0.5.3-beta.167 restores it.

#### Fixed

- The `cc` crate copied rustc's PGO flag onto clang, so Apple clang instrumented
  the bundled `lz4.c`. Its profile format no longer matches the one rustc 1.99
  writes, and the instrumented binary failed with "Runtime and instrumentation
  version mismatch". The lz4 build no longer takes rustc's flags.
- zlib links the system library again where there is one (always on macOS)
  instead of being compiled with the same inherited flags. The Linux release
  lanes still link it statically.
