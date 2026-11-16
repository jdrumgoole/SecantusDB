### The Linux release binaries link the C++ runtime statically

Since the Rust servers began collating with mongod's own ICU 57.1, the Linux
`secantusd-pg` and `secantusd-mdb` binaries needed `libstdc++.so.6` on the
machine running them. The release's dependency check refused the
`secantusd-pg-v0.1.0-beta.18` build for it, so that tag has no binaries.

#### Fixed

- `secantus-icu` links `libstdc++` statically on glibc Linux, as `mongod`
  does, when the compiler ships `libstdc++.a`; the `pg-oracle` CI lane now
  fails if `secantusd-pg` links it dynamically.
