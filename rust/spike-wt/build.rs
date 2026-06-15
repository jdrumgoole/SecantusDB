//! Spike 2 build glue — link the vendored WiredTiger C library and generate
//! FFI bindings from its (CMake-generated) public header.
//!
//! WiredTiger has no usable Rust crate, so the production core will FFI into
//! the exact C library we already vendor. This build script proves that path:
//!
//!   * If `WT_BUILD_DIR` is set and contains `libwiredtiger.a` +
//!     `include/wiredtiger.h`, reuse it (fast iteration; avoids a 5–10 min WT
//!     rebuild under cargo). This is how the spike is normally driven.
//!   * Otherwise, drive WT's own CMake build into `OUT_DIR/wt-build` so the
//!     spike is self-contained on a fresh checkout.
//!
//! Then `bindgen` turns `wiredtiger.h` into Rust declarations and we link the
//! static lib plus its system dependencies.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest.parent().unwrap().parent().unwrap().to_path_buf();
    let wt_src = repo_root.join("vendor/wiredtiger");

    let build_dir = match env::var_os("WT_BUILD_DIR") {
        Some(d) => PathBuf::from(d),
        None => build_wiredtiger(&wt_src),
    };

    let lib = build_dir.join("libwiredtiger.a");
    let header = build_dir.join("include/wiredtiger.h");
    assert!(
        lib.exists(),
        "libwiredtiger.a not found at {}",
        lib.display()
    );
    assert!(
        header.exists(),
        "wiredtiger.h not found at {}",
        header.display()
    );

    // Link the static lib + WT's runtime system deps.
    println!("cargo:rustc-link-search=native={}", build_dir.display());
    println!("cargo:rustc-link-lib=static=wiredtiger");
    for sys in ["pthread", "dl", "m"] {
        println!("cargo:rustc-link-lib=dylib={sys}");
    }
    println!("cargo:rerun-if-changed={}", header.display());
    println!("cargo:rerun-if-env-changed=WT_BUILD_DIR");

    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .allowlist_function("wiredtiger_open")
        .allowlist_function("wiredtiger_strerror")
        .allowlist_type("WT_CONNECTION")
        .allowlist_type("WT_SESSION")
        .allowlist_type("WT_CURSOR")
        .allowlist_var("WT_NOTFOUND")
        .layout_tests(false)
        .generate()
        .expect("bindgen failed on wiredtiger.h");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("wt_bindings.rs"))
        .expect("write bindings");
}

fn build_wiredtiger(wt_src: &Path) -> PathBuf {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("wt-build");
    std::fs::create_dir_all(&out).unwrap();

    // Mirror the project's wheel build: kill -Werror so modern toolchains
    // don't fail WT's strict targets.
    let _ = Command::new("python3")
        .arg(wt_src.join("../../cmake/patch_wt_strict.py"))
        .arg(wt_src.join("cmake/strict/strict_flags_helpers.cmake"))
        .status();

    let status = Command::new("cmake")
        .current_dir(&out)
        .args([
            "-G",
            "Ninja",
            "-DCMAKE_BUILD_TYPE=Release",
            "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
            "-DENABLE_STATIC=ON",
            "-DWITH_PIC=ON",
            "-DENABLE_SHARED=OFF",
            "-DENABLE_PYTHON=OFF",
            "-DENABLE_CPPSUITE=OFF",
        ])
        .arg(wt_src)
        .status()
        .expect("cmake configure");
    assert!(status.success(), "WT cmake configure failed");

    let status = Command::new("ninja")
        .current_dir(&out)
        .arg("wiredtiger_static")
        .status()
        .expect("ninja build");
    assert!(status.success(), "WT static build failed");
    out
}
