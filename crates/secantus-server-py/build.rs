//! Build script for the `_secantus_server` PyO3 extension.
//!
//! Same macOS concern as `secantus-storage-py`: under a plain `cargo build` (as
//! the wheel's CMake does, rather than via maturin) the macOS linker must be told
//! the CPython API symbols are resolved at import time by the host interpreter.
//! maturin injects `-undefined dynamic_lookup` for its own builds; for the
//! cargo-driven build we emit it ourselves. Only macOS needs it.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
