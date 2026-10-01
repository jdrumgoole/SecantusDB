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
    // docs.rs only runs rustdoc, which links nothing, and its sandbox has
    // neither the time nor the toolchain to compile WiredTiger.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // Every non-Windows WiredTiger -- the bundled build AND a prebuilt one
    // linked through the override -- has the builtin lz4 extension, so the lz4
    // block API is always needed.
    if target_os != "windows" {
        compile_lz4(&manifest);
    }

    println!("cargo:rerun-if-env-changed=SECANTUS_WT_INCLUDE");
    println!("cargo:rerun-if-env-changed=SECANTUS_WT_LIB");
    if let (Ok(inc), Ok(lib)) = (env::var("SECANTUS_WT_INCLUDE"), env::var("SECANTUS_WT_LIB")) {
        emit(Path::new(&inc), Path::new(&lib));
        return;
    }

    let src = manifest.join("wiredtiger");
    if !src.join("CMakeLists.txt").exists() {
        // In a SecantusDB checkout that has not run the refresh script, use a
        // WiredTiger the repo's own CMake build produced. A packaged crate
        // always carries `wiredtiger/`, so this never runs from crates.io.
        if let Some(dir) = repo_wt_build(&manifest) {
            emit(&dir.join("include"), &dir);
            return;
        }
        panic!(
            "{} is missing. In a SecantusDB checkout run \
             `python scripts/wt_sys_refresh.py`, or set SECANTUS_WT_INCLUDE and \
             SECANTUS_WT_LIB to an existing WiredTiger build.",
            src.display()
        );
    }
    println!("cargo:rerun-if-changed=wiredtiger.sha256");

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
        // time; the symbols come from libz-sys and compile_lz4 at the link.
        let z_inc = env::var("DEP_Z_INCLUDE").expect("libz-sys exports its include dir");
        let lz4_inc = manifest.join("lz4");
        // WiredTiger's find step wants a library path too; it is recorded,
        // never linked, because the static archive links nothing itself.
        let marker = Path::new(&z_inc).join("zlib.h");
        cfg.define("ENABLE_ZLIB", "OFF")
            .define("HAVE_BUILTIN_EXTENSION_ZLIB", "ON")
            .define("HAVE_LIBZ", &marker)
            .define("HAVE_LIBZ_INCLUDES", &z_inc)
            .define("ENABLE_LZ4", "OFF")
            .define("HAVE_BUILTIN_EXTENSION_LZ4", "ON")
            .define("HAVE_LIBLZ4", lz4_inc.join("lz4.h"))
            .define("HAVE_LIBLZ4_INCLUDES", &lz4_inc);
    }
    let out = cfg.build();
    let build = out.join("build");
    emit(&build.join("include"), &build);
    // A multi-config generator (MSBuild, the Windows default) puts the
    // library under a per-configuration directory instead.
    println!(
        "cargo:rustc-link-search=native={}",
        build.join("Release").display()
    );

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

/// `<repo>/build/*/wt-build` holding a built WiredTiger, if there is one.
fn repo_wt_build(manifest: &Path) -> Option<PathBuf> {
    let root = manifest.parent()?.parent()?;
    let mut found: Vec<PathBuf> = std::fs::read_dir(root.join("build"))
        .ok()?
        .flatten()
        .map(|e| e.path().join("wt-build"))
        .filter(|d| {
            d.join("include/wiredtiger.h").exists()
                && ["libwiredtiger.a", "wiredtiger.lib"]
                    .iter()
                    .any(|n| d.join(n).exists())
        })
        .collect();
    found.sort();
    found.pop()
}

/// Compile lz4's block API (`lz4/lz4.c`, lz4 1.10.0, BSD-2-Clause) into a
/// static library. Only that file: WiredTiger's lz4 extension calls
/// `LZ4_compress_default` / `LZ4_decompress_safe` and nothing else, and the
/// rest of liblz4 (the frame format) carries a copy of xxhash whose unprefixed
/// `XXH*` symbols collide with libpg_query's own copy when both are linked into
/// one binary, as the PostgreSQL server does. That collision is why this is not
/// the `lz4-sys` crate.
fn compile_lz4(manifest: &Path) {
    let dir = manifest.join("lz4");
    println!("cargo:rerun-if-changed={}", dir.join("lz4.c").display());
    cc::Build::new()
        .file(dir.join("lz4.c"))
        .include(&dir)
        .opt_level(3)
        .warnings(false)
        .compile("secantus_lz4");
}
