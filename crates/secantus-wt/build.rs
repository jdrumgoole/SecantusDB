//! Build script for `secantus-wt`.
//!
//! Generates Rust bindings for the WiredTiger C API with `bindgen` and links the
//! vendored WiredTiger library. WiredTiger's location is resolved in this order:
//!
//!   1. Explicit override: `SECANTUS_WT_INCLUDE` (dir with `wiredtiger.h`) +
//!      `SECANTUS_WT_LIB` (dir with `libwiredtiger.a` / `.so`).
//!   2. A set of probed locations: the project's CMake build output
//!      (`build/*/wt-build`) and the dev sandbox's `/tmp/wt-build`.
//!
//! bindgen needs libclang; set `LIBCLANG_PATH` if it isn't auto-discovered.

use std::env;
use std::path::{Path, PathBuf};

fn lib_present(dir: &str) -> bool {
    Path::new(&format!("{dir}/libwiredtiger.a")).exists()
        || Path::new(&format!("{dir}/libwiredtiger.so")).exists()
}

fn resolve_wt() -> (String, String) {
    if let (Ok(inc), Ok(lib)) = (env::var("SECANTUS_WT_INCLUDE"), env::var("SECANTUS_WT_LIB")) {
        return (inc, lib);
    }

    // Probe well-known build outputs. The project's scikit-build CMake build
    // drops WiredTiger under build/<tag>/wt-build; the dev sandbox uses
    // /tmp/wt-build.
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let repo_root = Path::new(&manifest)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut candidates: Vec<String> = vec!["/tmp/wt-build".to_string()];
    if let Ok(entries) = std::fs::read_dir(repo_root.join("build")) {
        for e in entries.flatten() {
            candidates.push(e.path().join("wt-build").to_string_lossy().into_owned());
        }
    }

    for c in &candidates {
        let header = format!("{c}/include/wiredtiger.h");
        if Path::new(&header).exists() && lib_present(c) {
            return (format!("{c}/include"), c.clone());
        }
    }

    panic!(
        "WiredTiger not found. Set SECANTUS_WT_INCLUDE (dir with wiredtiger.h) and \
         SECANTUS_WT_LIB (dir with libwiredtiger.a/.so), or build the vendored \
         WiredTiger. Probed: {candidates:?}"
    );
}

fn main() {
    let (inc, lib) = resolve_wt();

    println!("cargo:rerun-if-env-changed=SECANTUS_WT_INCLUDE");
    println!("cargo:rerun-if-env-changed=SECANTUS_WT_LIB");
    println!("cargo:rustc-link-search=native={lib}");
    println!("cargo:rustc-link-lib=static=wiredtiger");
    // WiredTiger's own system dependencies, per target OS. Mirrors the libs WT's
    // CMake detects and links (cmake/configs/auto.cmake `config_lib` +
    // per-OS config.cmake):
    //   - Linux:   pthread + rt + dl  (all three resolved by find_library)
    //   - macOS:   pthread + dl        (no librt on Darwin; pthread/dl are stubs
    //                                   in libSystem, so the links are harmless)
    //   - Windows: none beyond the MSVC default libs — WT's win port (WT_POSIX
    //              OFF) uses Win32 APIs the CRT's default-lib directives pull in.
    // CARGO_CFG_TARGET_OS is set by cargo to the *target* OS (correct under
    // cross-compilation too), unlike a host-evaluated cfg!.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let sys_libs: &[&str] = match target_os.as_str() {
        "linux" => &["pthread", "rt", "dl"],
        "macos" => &["pthread", "dl"],
        "windows" => &[],
        // Other POSIX targets (the BSDs etc.): pthread is the safe baseline.
        _ => &["pthread"],
    };
    for l in sys_libs {
        println!("cargo:rustc-link-lib=dylib={l}");
    }

    let header = format!("{inc}/wiredtiger.h");
    println!("cargo:rerun-if-changed={header}");
    let bindings = bindgen::Builder::default()
        .header(&header)
        .allowlist_function("wiredtiger_open")
        .allowlist_function("wiredtiger_strerror")
        .allowlist_type("WT_CONNECTION")
        .allowlist_type("WT_SESSION")
        .allowlist_type("WT_CURSOR")
        .allowlist_type("WT_ITEM")
        .allowlist_var("WT_.*")
        .generate()
        .expect("bindgen failed to generate WiredTiger bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("wt_sys.rs"))
        .expect("failed to write WiredTiger bindings");
}
