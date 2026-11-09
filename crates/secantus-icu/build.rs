//! Build script for `secantus-icu`.
//!
//! Compiles the ICU 57.1 that `mongod` 8.2 links -- mongo's own copy,
//! `vendor/icu4c-57.1` (from `mongodb/mongo` r8.2.11,
//! `src/third_party/icu4c-57.1`) -- into static libraries, with the source
//! lists, defines and warning switches of mongo's `source/BUILD.bazel`. The
//! lists are read from that file, so what is compiled is what mongod compiles.
//!
//! The ICU data is not compiled: `src/lib.rs` embeds mongo's trimmed
//! `icudt57l.dat` (`icudt57b.dat` on a big-endian target) and hands it to
//! `udata_setCommonData`, as mongod's generated `icu_init.cpp` does.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// mongo's `ICU4C_DEFINES`.
const DEFINES: &[(&str, &str)] = &[
    ("UCONFIG_NO_BREAK_ITERATION", "1"),
    ("UCONFIG_NO_FORMATTING", "1"),
    ("UCONFIG_NO_TRANSLITERATION", "1"),
    ("UCONFIG_NO_REGULAR_EXPRESSIONS", "1"),
    ("U_CHARSET_IS_UTF8", "1"),
    ("U_STATIC_IMPLEMENTATION", "1"),
    ("U_USING_ICU_NAMESPACE", "0"),
];

/// mongo's `ICU4C_COPTS` for gcc / clang.
const GNU_FLAGS: &[&str] = &[
    "-w",
    "-Wno-sign-compare",
    "-Wno-deprecated-volatile",
    "-Wno-implicit-fallthrough",
    "-Wno-ambiguous-reversed-operator",
    "-Wno-deprecated-anon-enum-enum-conversion",
    "-Wno-deprecated-array-compare",
];

/// mongo's `ICU4C_COPTS` for MSVC.
const MSVC_FLAGS: &[&str] = &["/wd4996", "/wd5056"];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = icu_root(&manifest);
    let source = root.join("source");
    let bazel = source.join("BUILD.bazel");
    println!("cargo:rerun-if-changed={}", bazel.display());
    println!("cargo:rerun-if-changed=shim/shim.cpp");

    let big_endian = env::var("CARGO_CFG_TARGET_ENDIAN").as_deref() == Ok("big");
    let dat = source.join("mongo_sources").join(if big_endian {
        "icudt57b.dat"
    } else {
        "icudt57l.dat"
    });
    assert!(dat.exists(), "{} is missing", dat.display());
    println!("cargo:rerun-if-changed={}", dat.display());
    println!("cargo:rustc-env=SECANTUS_ICU_DAT={}", dat.display());

    // docs.rs only runs rustdoc, which links nothing.
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

    let text = fs::read_to_string(&bazel).expect("read BUILD.bazel");
    let includes = [source.join("common"), source.join("i18n")];
    // Dependencies first in the build, last on the link line: cc emits each
    // library as it is compiled, and a static link resolves left to right.
    compile(
        "secantus_icu_shim",
        &[manifest.join("shim/shim.cpp")],
        &includes,
        None,
    );
    for (target, local) in [
        ("icu_i18n", Some("U_I18N_IMPLEMENTATION")),
        ("icu_common", Some("U_COMMON_IMPLEMENTATION")),
        ("icu_data", None),
    ] {
        let files: Vec<PathBuf> = srcs(&text, target)
            .into_iter()
            .map(|f| source.join(f))
            .collect();
        assert!(!files.is_empty(), "no srcs for {target} in BUILD.bazel");
        for f in &files {
            println!("cargo:rerun-if-changed={}", f.display());
        }
        let (c, cpp): (Vec<_>, Vec<_>) = files
            .into_iter()
            .partition(|f| f.extension().is_some_and(|e| e == "c"));
        if !cpp.is_empty() {
            compile(&format!("secantus_{target}_cpp"), &cpp, &includes, local);
        }
        if !c.is_empty() {
            compile(&format!("secantus_{target}_c"), &c, &includes, local);
        }
    }
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "linux" && target_env == "gnu" {
        println!("cargo:rustc-link-lib=dylib=dl");
    }
    if target_os == "windows" {
        // `common/wintz.c` reads the time-zone names from the registry.
        println!("cargo:rustc-link-lib=dylib=advapi32");
    }
}

/// The vendored tree: packaged into the crate (`icu4c-57.1/`) or, in a
/// SecantusDB checkout, the repository's `vendor/icu4c-57.1`.
fn icu_root(manifest: &Path) -> PathBuf {
    let packaged = manifest.join("icu4c-57.1");
    if packaged.join("source").exists() {
        return packaged;
    }
    let repo = manifest.join("../../vendor/icu4c-57.1");
    if repo.join("source").exists() {
        return repo;
    }
    panic!(
        "ICU 57.1 sources not found at {} or {}",
        packaged.display(),
        repo.display()
    );
}

/// The `srcs` of one `mongo_cc_library` in mongo's BUILD.bazel.
fn srcs(text: &str, target: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_target = false;
    let mut in_srcs = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix("name = \"") {
            in_target = name.trim_end_matches(['"', ',']) == target;
            in_srcs = false;
            continue;
        }
        if !in_target {
            continue;
        }
        if t == "srcs = [" {
            in_srcs = true;
        } else if in_srcs && t.starts_with(']') {
            return out;
        } else if in_srcs {
            out.push(t.trim_matches(|c| c == '"' || c == ',').to_string());
        }
    }
    out
}

fn compile(lib: &str, files: &[PathBuf], includes: &[PathBuf], local: Option<&str>) {
    let cpp = files
        .iter()
        .any(|f| f.extension().is_some_and(|e| e == "cpp"));
    let mut b = cc::Build::new();
    // Do NOT copy rustc's flags onto the C++ compile: `cc` would turn the
    // release build's `-Cprofile-generate` / `-Cprofile-use` into clang's
    // `-fprofile-*`, which Apple clang and rustc's LLVM do not share a format
    // for (see secantus-wiredtiger-sys's lz4 build).
    b.inherit_rustflags(false)
        .files(files)
        .includes(includes)
        .opt_level(2)
        .warnings(false)
        .cpp(cpp);
    if cpp {
        b.std("c++17");
    }
    for (k, v) in DEFINES {
        b.define(k, Some(*v));
    }
    if let Some(d) = local {
        b.define(d, Some("1"));
    }
    let flags = if b.get_compiler().is_like_msvc() {
        MSVC_FLAGS
    } else {
        GNU_FLAGS
    };
    for f in flags {
        b.flag_if_supported(f);
    }
    b.compile(lib);
}
