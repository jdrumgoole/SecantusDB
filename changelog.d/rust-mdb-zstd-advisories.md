### Rust MongoDB server: zstd 0.14.3, clearing RUSTSEC-2026-0391 / 0398 / 0403 / 0410

The server's wire compressor moves from zstd 0.13.3 (zstd-safe 7.3.0,
zstd-sys 2.1.1) to zstd 0.14.3 (zstd-safe 8.2.0, zstd-sys 2.1.2), which
`cargo audit` now requires.

#### Changed

- `secantus-wire` depends on zstd 0.14.
- The official Rust driver used by `crates/secantusdb/tests/compression.rs`
  is built without its `zstd-compression` feature: its newest release
  (3.9.1) still pins zstd 0.13. zstd is covered there over raw
  `OP_COMPRESSED` frames instead
  (`raw_frames_round_trip_crud_and_a_large_document_under_zstd`, the driver
  test's workload); snappy and zlib still go through the driver.
