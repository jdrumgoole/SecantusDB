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
    // Unix static/shared names, plus MSVC's `wiredtiger.lib` (the R7 tail:
    // the Windows wheel build produces neither `lib*` name, so the standalone
    // binary never resolved WT there) and macOS's `.dylib` for completeness.
    [
        "libwiredtiger.a",
        "libwiredtiger.so",
        "libwiredtiger.dylib",
        "wiredtiger.lib",
    ]
    .iter()
    .any(|n| Path::new(&format!("{dir}/{n}")).exists())
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
         SECANTUS_WT_LIB (dir with libwiredtiger.a/.so/.dylib or wiredtiger.lib), \
         or build the vendored WiredTiger. Probed: {candidates:?}"
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
    // `z` (zlib): WT is built with the builtin zlib block-compressor extension
    // (HAVE_BUILTIN_EXTENSION_ZLIB), whose `zlib_compress.c.o` — now inside
    // libwiredtiger_static — references libz's inflate/deflate. libz is a system
    // library on macOS (SDK) and Linux (manylinux/musl ship zlib).
    let sys_libs: &[&str] = match target_os.as_str() {
        "linux" => &["pthread", "rt", "dl", "z", "lz4"],
        "macos" => &["pthread", "dl", "z", "lz4"],
        "windows" => &[],
        // Other POSIX targets (the BSDs etc.): pthread is the safe baseline.
        _ => &["pthread"],
    };
    for l in sys_libs {
        println!("cargo:rustc-link-lib=dylib={l}");
    }

    // lz4 is in `sys_libs` above because it is the default block compressor and
    // WiredTiger's builtin extension references it. zlib stays linked too: a
    // store created before the lz4 switch has zlib tables, and
    // `block_compressor` is recorded at create time, so dropping zlib would
    // make existing data unreadable.
    //
    // `SECANTUS_WT_EXTRA_COMPRESSORS=1` matches the CMake option of the same
    // name and adds snappy + zstd, which are opt-in only.
    // `SECANTUS_WT_EXTRA_LIBDIR` adds a search path (e.g. Homebrew's
    // /opt/homebrew/lib) for any of them.
    println!("cargo:rerun-if-env-changed=SECANTUS_WT_EXTRA_COMPRESSORS");
    println!("cargo:rerun-if-env-changed=SECANTUS_WT_EXTRA_LIBDIR");
    if let Ok(dir) = env::var("SECANTUS_WT_EXTRA_LIBDIR") {
        println!("cargo:rustc-link-search=native={dir}");
    } else if target_os == "macos" {
        // liblz4 is a default link library now, and Apple ships none in the
        // SDK, so the search path has to be found rather than assumed.
        //
        // The wheel build supplies its own static liblz4 via
        // SECANTUS_WT_EXTRA_LIBDIR: `brew install lz4` produces a dylib
        // targeting the runner's OS (macOS 14), which `delocate` refuses to
        // bundle into a wheel targeting macOS 11 — see
        // tools/build_lz4_macos.sh. Homebrew's prefixes are the fallback for a
        // plain developer `cargo build`, where no wheel is produced and the
        // deployment target does not matter.
        // The wheel build points SECANTUS_WT_EXTRA_LIBDIR at its own static
        // build (handled above); these are the developer fallbacks.
        for prefix in ["/opt/homebrew/lib", "/usr/local/lib"] {
            let has_static = std::path::Path::new(&format!("{prefix}/liblz4.a")).exists();
            let has_dylib = std::path::Path::new(&format!("{prefix}/liblz4.dylib")).exists();
            if has_static || has_dylib {
                println!("cargo:rustc-link-search=native={prefix}");
                break;
            }
        }
    }
    if env::var_os("SECANTUS_WT_EXTRA_COMPRESSORS").is_some() && target_os != "windows" {
        for l in ["snappy", "zstd"] {
            println!("cargo:rustc-link-lib=dylib={l}");
        }
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
