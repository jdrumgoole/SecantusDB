//! The snappy and zstd libraries mongod 8.2 links, for WiredTiger's builtin
//! compressor extensions.
//!
//! This crate has no Rust API. Depending on it links mongo's snappy 1.1.10
//! and zstd 1.5.5 (compiled by `build.rs` from `vendor/snappy` and
//! `vendor/zstandard`), which WiredTiger's `snappy_compress.c` and
//! `zstd_compress.c` call. A crate that links WiredTiger names it with
//! `extern crate secantus_compressors;` so the libraries reach the link.
#![no_std]

extern "C" {
    fn snappy_max_compressed_length(source_length: usize) -> usize;
    fn ZSTD_versionNumber() -> u32;
}

/// The snappy library's worst-case compressed size for `len` bytes. Exists so
/// a test can prove the library is linked and is mongo's.
pub fn snappy_max_compressed_length_of(len: usize) -> usize {
    // SAFETY: a pure function of its argument.
    unsafe { snappy_max_compressed_length(len) }
}

/// The linked zstd's version as `major * 10000 + minor * 100 + patch`.
pub fn zstd_version() -> u32 {
    // SAFETY: returns a constant.
    unsafe { ZSTD_versionNumber() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mongos_libraries_are_linked() {
        // snappy's documented bound: 32 + n + n / 6.
        assert_eq!(super::snappy_max_compressed_length_of(600), 32 + 600 + 100);
        assert_eq!(super::zstd_version(), 10505);
    }
}
