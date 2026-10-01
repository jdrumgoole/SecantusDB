//! Build script for `secantus-wiredtiger-sys`.
//!
//! Compiles the bundled, pre-patched WiredTiger (`wiredtiger/`, written by
//! `scripts/wt_sys_refresh.py`) with CMake into a static library, the same
//! configuration as the wheel's ExternalProject in the top-level
//! `CMakeLists.txt`: static, PIC, Release, zlib and lz4 built in, Python /
//! SWIG / cppsuite off. Needs CMake and a C compiler, and nothing else.
//!
//! `SECANTUS_WT_INCLUDE` + `SECANTUS_WT_LIB` link an existing build instead.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=SECANTUS_WT_INCLUDE");
    println!("cargo:rerun-if-env-changed=SECANTUS_WT_LIB");
    if let (Ok(inc), Ok(lib)) = (env::var("SECANTUS_WT_INCLUDE"), env::var("SECANTUS_WT_LIB")) {
        emit(Path::new(&inc), Path::new(&lib));
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("wiredtiger");
    if !src.join("CMakeLists.txt").exists() {
        panic!(
            "{} is missing. In a SecantusDB checkout run \
             `python scripts/wt_sys_refresh.py`, or set SECANTUS_WT_INCLUDE and \
             SECANTUS_WT_LIB to an existing WiredTiger build.",
            src.display()
        );
    }
    println!("cargo:rerun-if-changed=wiredtiger.sha256");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let mut cfg = cmake::Config::new(&src);
    cfg.profile("Release")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        .define("ENABLE_STATIC", "ON")
        .define("ENABLE_SHARED", "OFF")
        .define("ENABLE_PYTHON", "OFF")
        .define("ENABLE_CPPSUITE", "OFF")
        .define("ENABLE_LLVM", "OFF")
        // The optional extensions and libraries WiredTiger otherwise picks up
        // from whatever the host has installed. The wheel links none of them;
        // detecting them here would make the build depend on the machine.
        .define("ENABLE_SNAPPY", "OFF")
        .define("ENABLE_ZSTD", "OFF")
        .define("ENABLE_SODIUM", "OFF")
        .define("ENABLE_TCMALLOC", "OFF")
        .define("ENABLE_MEMKIND", "OFF")
        .build_target("wiredtiger_static");
    if target_os != "windows" {
        // Windows builds WiredTiger with no compressors, as the wheel does.
        // Elsewhere the builtin extensions only need the HEADERS at configure
        // time; the symbols come from libz-sys / lz4-sys at the final link.
        let z_inc = env::var("DEP_Z_INCLUDE").expect("libz-sys exports its include dir");
        let lz4_inc = env::var("DEP_LZ4_INCLUDE").expect("lz4-sys exports its include dir");
        // WiredTiger's find step wants a library path too; it is recorded,
        // never linked, because the static archive links nothing itself.
        let marker = Path::new(&z_inc).join("zlib.h");
        cfg.define("ENABLE_ZLIB", "OFF")
            .define("HAVE_BUILTIN_EXTENSION_ZLIB", "ON")
            .define("HAVE_LIBZ", &marker)
            .define("HAVE_LIBZ_INCLUDES", &z_inc)
            .define("ENABLE_LZ4", "OFF")
            .define("HAVE_BUILTIN_EXTENSION_LZ4", "ON")
            .define("HAVE_LIBLZ4", Path::new(&lz4_inc).join("lz4.h"))
            .define("HAVE_LIBLZ4_INCLUDES", &lz4_inc);
    }
    let out = cfg.build();
    let build = out.join("build");
    emit(&build.join("include"), &build);

    let sys_libs: &[&str] = match target_os.as_str() {
        "linux" => &["pthread", "rt", "dl"],
        "macos" => &["pthread", "dl"],
        "windows" => &[],
        _ => &["pthread"],
    };
    for l in sys_libs {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
}

fn emit(include: &Path, lib: &Path) {
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=wiredtiger");
    println!("cargo:include={}", include.display());
    println!("cargo:lib={}", lib.display());
}
