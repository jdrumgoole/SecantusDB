//! Build script for the `_secantus_server` PyO3 extension.
//!
//! Same macOS concern as `secantus-storage-py`: under a plain `cargo build` (as
//! the wheel's CMake does, rather than via maturin) the macOS linker must be told
//! the CPython API symbols are resolved at import time by the host interpreter.
//! maturin injects `-undefined dynamic_lookup` for its own builds; for the
//! cargo-driven build we emit it ourselves. Only macOS needs it.

/// Stamp the git tree hash of the sources this extension is built from, so a
/// test run can tell whether the installed `.so` matches the checkout.
///
/// Mirrors `secantus-core-py`'s stamp; see that crate for why a TREE hash and
/// not `git rev-parse HEAD` (which moves on every commit and would cry stale
/// constantly) or `CARGO_PKG_VERSION` (which moves at release cadence and was
/// measured reporting the same version for a build stale enough to need
/// rebuilding twice in a day).
///
/// **Why the whole `crates` tree here, where core-py names two crates.** This
/// extension links `secantus-storage`, `-storage-adapter`, `-commands`,
/// `-server` and `-pgserver`, which transitively covers nearly the whole
/// workspace, so enumerating them would be a list that silently goes stale when
/// a dependency is added. `HEAD:crates` is what the `secantusd-rs` and
/// `secantusd-pg` binaries already stamp, so the three agree. The cost is that
/// an unrelated crate's change reads as stale here; the benefit is that a
/// missing dependency edge cannot make the check lie.
///
/// Degrades to empty, never fails the build: no git means no stamp, and the
/// check treats an unstamped extension as "cannot tell" and stays quiet.
fn stamp_source_tree() {
    // Rebuild the stamp when the committed content could have moved. Without
    // this, cargo caches build.rs output and the stamp goes stale on its own —
    // a staleness checker that is itself stale.
    println!("cargo:rerun-if-changed=src");
    // Resolved through git, NOT as `../../.git/HEAD`. In a WORKTREE `.git` is
    // a FILE pointing elsewhere, so that literal path does not exist, cargo
    // never sees it change, and the build script is never re-run -- the stamp
    // then reports the tree of whatever checkout last built it. A staleness
    // checker that is itself stale, which is the failure this block exists to
    // prevent. Found 2026-09-28 when a freshly built extension in a worktree
    // reported the MAIN checkout's tree.
    for path in ["HEAD", "index"] {
        if let Some(resolved) = std::process::Command::new("git")
            .args(["rev-parse", "--git-path", path])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|p| !p.is_empty())
        {
            println!("cargo:rerun-if-changed={resolved}");
        }
    }

    let stamp = std::process::Command::new("git")
        .args(["rev-parse", "HEAD:crates"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=SECANTUS_SOURCE_TREE={stamp}");
}

fn main() {
    stamp_source_tree();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
