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
    // `SECANTUS_WT_STATIC_COMPRESSORS=1` links zlib and lz4 from their static
    // archives instead of the system shared objects, so the resulting binary
    // needs nothing but libc. It is OPT-IN rather than the default because the
    // manylinux / musl WHEEL lanes are happy with shared compressors —
    // auditwheel bundles them — and flipping the default would silently change
    // what those wheels contain. The standalone-binary release lanes set it;
    // see .github/workflows/release-binaries.yml. Requires the `.a` archives
    // (`zlib1g-dev` + `liblz4-dev` on Debian) to be installed.
    println!("cargo:rerun-if-env-changed=SECANTUS_WT_STATIC_COMPRESSORS");
    let static_compressors = env::var_os("SECANTUS_WT_STATIC_COMPRESSORS").is_some();
    let sys_libs: &[&str] = match target_os.as_str() {
        // zlib and lz4 are absent here: both are compressors, and their link
        // KIND is decided below rather than forced to dylib.
        "linux" => &["pthread", "rt", "dl"],
        // lz4 is absent here on purpose too: macOS picks its link KIND below,
        // in the same place it picks the search path, because it is the target
        // where a dylib cannot be assumed to exist on the machine that runs the
        // binary. zlib IS a system library there (the macOS SDK ships it).
        "macos" => &["pthread", "dl", "z"],
        "windows" => &[],
        // Other POSIX targets (the BSDs etc.): pthread is the safe baseline.
        _ => &["pthread"],
    };
    for l in sys_libs {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
    if target_os == "linux" {
        // `z` (zlib) and `lz4`: WT is built with both as BUILTIN block-compressor
        // extensions (HAVE_BUILTIN_EXTENSION_ZLIB / _LZ4), so `zlib_compress.c.o`
        // and `lz4_compress.c.o` sit inside libwiredtiger_static and reference
        // each library's symbols. Which is why they are linked at all.
        let kind = if static_compressors {
            "static"
        } else {
            "dylib"
        };
        for l in ["z", "lz4"] {
            println!("cargo:rustc-link-lib={kind}={l}");
        }
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
    let extra_libdir = env::var("SECANTUS_WT_EXTRA_LIBDIR").ok();
    if let Some(dir) = extra_libdir.as_deref() {
        println!("cargo:rustc-link-search=native={dir}");
    }
    if target_os == "macos" {
        // liblz4 is a default link library now, and Apple ships none in the
        // SDK, so both the search path AND the link kind have to be worked out
        // rather than assumed.
        //
        // The wheel build supplies its own static liblz4 via
        // SECANTUS_WT_EXTRA_LIBDIR: `brew install lz4` produces a dylib
        // targeting the runner's OS (macOS 14), which `delocate` refuses to
        // bundle into a wheel targeting macOS 11 — see
        // tools/build_lz4_macos.sh. Homebrew's prefixes are the fallback for a
        // plain developer `cargo build`.
        //
        // PREFER THE STATIC ARCHIVE WHEREVER ONE EXISTS. `dylib=lz4` used to be
        // emitted unconditionally, and because Homebrew ships liblz4.a and
        // liblz4.dylib side by side the linker always took the dylib — by its
        // ABSOLUTE Homebrew path. That is invisible on a build machine (which
        // has Homebrew by definition) and fatal on a user's: both published
        // macOS binaries, secantusd-rs 0.5.3-beta.164 and secantusd-pg
        // 0.1.0-beta.1, carry
        //     /opt/homebrew/opt/lz4/lib/liblz4.1.dylib
        // and fail to launch on a Mac without Homebrew lz4 installed at exactly
        // that path. Nothing in the build says so; `otool -L` on the shipped
        // archive is what says so.
        let search: Vec<&str> = match extra_libdir.as_deref() {
            Some(dir) => vec![dir],
            None => vec!["/opt/homebrew/lib", "/usr/local/lib"],
        };
        let mut kind = "dylib";
        for prefix in search {
            let has_static = std::path::Path::new(&format!("{prefix}/liblz4.a")).exists();
            let has_dylib = std::path::Path::new(&format!("{prefix}/liblz4.dylib")).exists();
            if has_static || has_dylib {
                if extra_libdir.is_none() {
                    println!("cargo:rustc-link-search=native={prefix}");
                }
                if has_static {
                    kind = "static";
                }
                break;
            }
        }
        println!("cargo:rustc-link-lib={kind}=lz4");
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
