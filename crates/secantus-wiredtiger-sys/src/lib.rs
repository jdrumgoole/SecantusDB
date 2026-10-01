//! WiredTiger, built from bundled source as a static library.
//!
//! This crate only builds and links `libwiredtiger`; it exposes no Rust API.
//! Dependents read the header location from the `DEP_WIREDTIGER_INCLUDE`
//! build-script variable (and the library directory from
//! `DEP_WIREDTIGER_LIB`). `secantus-wt` is the safe wrapper over it.
//!
//! Set `SECANTUS_WT_INCLUDE` and `SECANTUS_WT_LIB` to link a WiredTiger you
//! built yourself instead; the source build is then skipped.
//!
//! Internal to SecantusDB: there is no semver promise.

// Referenced so the linker keeps the bundled compressors WiredTiger's builtin
// zlib and lz4 extensions call into.
extern crate libz_sys;
extern crate lz4_sys;
