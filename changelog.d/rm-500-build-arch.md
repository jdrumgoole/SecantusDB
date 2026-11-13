### Rust MongoDB server: `buildInfo` names macOS arm64 as `arm64`

`buildInfo`'s `buildEnvironment` and the `Build Info` startup line now name
the architecture as mongod's own build for the platform does. On macOS arm64
mongod 8.2.11 says `arm64`; this server said Rust's `aarch64`.

#### Fixed

- `buildEnvironment.distarch` and `target_arch` (and the `environment` of
  the `Build Info` log line, which reads them) are `arm64` on macOS arm64.
  Linux names were already mongod's (`x86_64`, `aarch64`).

Fixes RM-500 (tests/backlog/test_rm_found_2026_11_10.py::test_build_info_names_the_architecture_as_mongod_does)
