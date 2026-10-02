### The macOS release binary builds again, for real this time

0.5.3-beta.166 and 0.5.3-beta.167 published no macOS `secantusd-rs`.
0.5.3-beta.168 restores it.

#### Fixed

- The macOS profile-guided build instrumented C as well as Rust. The `cc`
  crate copies rustc's profiling flags onto clang, so `ring` (used by rustls)
  was instrumented by Apple clang. Apple clang's profile format no longer
  matches the one rustc 1.99 reads. The macOS release build now compiles C
  through a wrapper that drops those flags, so only Rust is profiled. The
  0.5.3-beta.167 fix covered only the bundled lz4, which was not enough.
