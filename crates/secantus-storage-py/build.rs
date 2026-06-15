//! Build script for the `_secantus_storage` PyO3 extension.
//!
//! When this extension is built with a plain `cargo build` (as the wheel's CMake
//! does under `SECANTUS_BUILD_STORAGE_ENGINE=ON`, rather than via maturin), the
//! macOS linker needs to be told that the CPython API symbols (`_PyList_Append`,
//! `_PyModule_Create2`, `__Py_DecRef`, ...) are resolved at *import time* by the
//! host interpreter — otherwise the link fails with "Undefined symbols ... _Py*".
//! maturin injects `-undefined dynamic_lookup` for its own builds; for the
//! cargo-driven build we emit it ourselves.
//!
//! Only macOS needs this: Linux resolves undefined symbols in shared objects at
//! load time by default, and the Windows build links the abi3 `python3` import
//! library — both link cleanly without the flag.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
