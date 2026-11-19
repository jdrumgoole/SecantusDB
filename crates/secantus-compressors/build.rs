//! Build script for `secantus-compressors`.
//!
//! Compiles the snappy and zstd that `mongod` 8.2 links -- mongo's own copies,
//! `vendor/snappy` and `vendor/zstandard` (from `mongodb/mongo` r8.2.11,
//! `src/third_party/`) -- into two static libraries, with the source lists,
//! include directories and defines of mongo's `BUILD.bazel` files.
//!
//! Publishes the include directories to dependants (`links =
//! "secantus_compressors"`): `secantus-wiredtiger-sys` configures WiredTiger's
//! builtin snappy and zstd extensions with them.
use std::env;
use std::path::{Path, PathBuf};

/// mongo's `snappy` srcs (`vendor/snappy/BUILD.bazel`), the `.cc` files.
const SNAPPY_SOURCES: &[&str] = &[
    "dist/snappy.cc",
    "dist/snappy-c.cc",
    "dist/snappy-sinksource.cc",
];

/// mongo's `zstd` srcs (`vendor/zstandard/BUILD.bazel`), the `.c` files.
const ZSTD_SOURCES: &[&str] = &[
    "zstd/lib/common/entropy_common.c",
    "zstd/lib/common/error_private.c",
    "zstd/lib/common/fse_decompress.c",
    "zstd/lib/common/pool.c",
    "zstd/lib/common/threading.c",
    "zstd/lib/common/xxhash.c",
    "zstd/lib/common/zstd_common.c",
    "zstd/lib/compress/fse_compress.c",
    "zstd/lib/compress/hist.c",
    "zstd/lib/compress/huf_compress.c",
    "zstd/lib/compress/zstd_compress.c",
    "zstd/lib/compress/zstd_compress_literals.c",
    "zstd/lib/compress/zstd_compress_sequences.c",
    "zstd/lib/compress/zstd_compress_superblock.c",
    "zstd/lib/compress/zstd_double_fast.c",
    "zstd/lib/compress/zstd_fast.c",
    "zstd/lib/compress/zstd_lazy.c",
    "zstd/lib/compress/zstd_ldm.c",
    "zstd/lib/compress/zstd_opt.c",
    "zstd/lib/compress/zstdmt_compress.c",
    "zstd/lib/decompress/huf_decompress.c",
    "zstd/lib/decompress/zstd_ddict.c",
    "zstd/lib/decompress/zstd_decompress.c",
    "zstd/lib/decompress/zstd_decompress_block.c",
    "zstd/lib/deprecated/zbuff_common.c",
    "zstd/lib/deprecated/zbuff_compress.c",
    "zstd/lib/deprecated/zbuff_decompress.c",
    "zstd/lib/dictBuilder/cover.c",
    "zstd/lib/dictBuilder/divsufsort.c",
    "zstd/lib/dictBuilder/fastcover.c",
    "zstd/lib/dictBuilder/zdict.c",
];

fn main() {
    // docs.rs only runs rustdoc, which links nothing.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let snappy = vendored(&manifest, "snappy");
    let zstd = vendored(&manifest, "zstandard");
    println!("cargo:rerun-if-changed={}", snappy.display());
    println!("cargo:rerun-if-changed={}", zstd.display());

    // On glibc Linux, link the C++ runtime statically, as secantus-icu does
    // (and mongod): a released binary must not need the host's libstdc++.
    let static_stdcxx = if target_os == "linux" && target_env == "gnu" {
        static_libstdcxx_dir()
    } else {
        None
    };

    // snappy: mongo's `includes` are `dist`, `platform/build_all` and the
    // target's platform directory, with `HAVE_CONFIG_H` defined.
    let platform = match (target_os.as_str(), target_arch.as_str()) {
        ("linux", "aarch64") => "build_linux_aarch64",
        ("linux", "x86_64") => "build_linux_x86_64",
        ("linux", "powerpc64") => "build_linux_ppc64le",
        ("linux", "s390x") => "build_linux_s390x",
        ("macos", "x86_64") => "build_macOS_x86_64",
        ("macos", "aarch64") => "build_macOS_aarch64",
        ("windows", _) => "build_windows",
        _ => "build_posix",
    };
    let snappy_includes = [
        snappy.join("dist"),
        snappy.join("platform/build_all"),
        snappy.join("platform").join(platform),
    ];
    let mut b = cc::Build::new();
    // Do NOT copy rustc's flags onto the C/C++ compile: `cc` would turn the
    // release build's `-Cprofile-generate` / `-Cprofile-use` into clang's
    // `-fprofile-*`, which Apple clang and rustc's LLVM do not share a format
    // for (see secantus-wiredtiger-sys's lz4 build).
    b.inherit_rustflags(false)
        .cpp(true)
        .std("c++17")
        .files(SNAPPY_SOURCES.iter().map(|s| snappy.join(s)))
        .includes(&snappy_includes)
        .define("HAVE_CONFIG_H", None)
        .opt_level(2)
        .warnings(false);
    if static_stdcxx.is_some() {
        // main() links the static archive instead.
        b.cpp_link_stdlib(None);
    }
    if target_arch == "x86_64" && !b.get_compiler().is_like_msvc() {
        // mongo's x86-64 config.h turns on the SSSE3 and CRC32 (SSE4.2)
        // paths, which need the instructions enabled: mongod 8.2 itself
        // requires a CPU with them.
        b.flag("-msse4.2");
    }
    if b.get_compiler().is_like_msvc() {
        b.flag_if_supported("/wd4018");
    } else {
        for f in [
            "-Wno-sign-compare",
            "-Wno-unused-function",
            "-Wno-ignored-attributes",
        ] {
            b.flag_if_supported(f);
        }
    }
    b.compile("secantus_snappy");

    // zstd: mongo's `includes` is `zstd/lib`; the x86-64 Huffman decoder in
    // assembly is compiled everywhere but Windows, as mongo's select does.
    let zstd_lib = zstd.join("zstd/lib");
    let mut z = cc::Build::new();
    z.inherit_rustflags(false)
        .files(ZSTD_SOURCES.iter().map(|s| zstd.join(s)))
        .include(&zstd_lib)
        .opt_level(2)
        .warnings(false);
    if target_os != "windows" {
        z.file(zstd.join("zstd/lib/decompress/huf_decompress_amd64.S"));
    }
    z.compile("secantus_zstd");

    if let Some(dir) = &static_stdcxx {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=stdc++");
    }

    // For dependants' build scripts: DEP_SECANTUS_COMPRESSORS_SNAPPY_INCLUDE
    // (the directory holding snappy-c.h) and ..._ZSTD_INCLUDE (zstd.h).
    println!("cargo:snappy_include={}", snappy.join("dist").display());
    println!("cargo:zstd_include={}", zstd_lib.display());
}

/// The vendored tree: packaged into the crate (`<name>/`) or, in a SecantusDB
/// checkout, the repository's `vendor/<name>`.
fn vendored(manifest: &Path, name: &str) -> PathBuf {
    let packaged = manifest.join(name);
    if packaged.join("BUILD.bazel").exists() {
        return packaged;
    }
    let repo = manifest.join("../../vendor").join(name);
    assert!(
        repo.join("BUILD.bazel").exists(),
        "neither {} nor {} holds mongo's {name}",
        packaged.display(),
        repo.display()
    );
    repo
}

/// The directory holding the C++ compiler's `libstdc++.a`, if it has one.
fn static_libstdcxx_dir() -> Option<PathBuf> {
    let compiler = cc::Build::new().cpp(true).get_compiler();
    let out = std::process::Command::new(compiler.path())
        .arg("-print-file-name=libstdc++.a")
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    // An unknown file name is echoed back bare, not as a path.
    if path.is_absolute() && path.is_file() {
        path.parent().map(Path::to_path_buf)
    } else {
        None
    }
}
