// A PyO3 `extension-module` cdylib resolves the CPython API symbols from the
// host interpreter at *runtime*, so they are intentionally absent at link time.
// On macOS the linker rejects undefined symbols by default and the link fails;
// it must be told to allow them with `-undefined dynamic_lookup`. maturin passes
// this automatically (so `invoke rust-build` and the rust-wheels CI work), but a
// plain `cargo build`/`cargo test`/`cargo clippy` over the workspace — used in
// local macOS dev and mirrored by the `rust` CI job — does not. Emitting the
// flag here scopes it to this crate's link, only on macOS; Linux shared objects
// allow undefined symbols by default and need nothing.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    }
}
