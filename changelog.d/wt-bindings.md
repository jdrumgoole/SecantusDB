### Committed WiredTiger bindings

`secantus-wt` can now build without libclang.

#### Added

- `secantus-wt` ships its WiredTiger bindings as `src/bindings.rs`. A build without the `bindgen` feature uses that file and needs no libclang.
- The `bindgen` feature, on by default, regenerates the bindings and fails the build if they differ from the committed file. `./inv wt-bindings-refresh` updates the file.
