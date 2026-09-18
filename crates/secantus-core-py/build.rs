// A PyO3 `extension-module` cdylib resolves the CPython API symbols from the
// host interpreter at *runtime*, so they are intentionally absent at link time.
// On macOS the linker rejects undefined symbols by default and the link fails;
// it must be told to allow them with `-undefined dynamic_lookup`. maturin passes
// this automatically (so `invoke rust-build` and the rust-wheels CI work), but a
// plain `cargo build`/`cargo test`/`cargo clippy` over the workspace — used in
// local macOS dev and mirrored by the `rust` CI job — does not. Emitting the
// flag here scopes it to this crate's link, only on macOS; Linux shared objects
// allow undefined symbols by default and need nothing.
/// Stamp the git tree hash of the sources this extension is built from, so a
/// test run can tell whether the installed `.so` matches the checkout.
///
/// **Why a TREE hash and not the commit SHA.** `git rev-parse HEAD` moves on
/// every commit, including the thousands that never touch these crates, so a
/// commit-SHA check would cry stale constantly and get switched off.
/// `HEAD:crates/secantus-core` is the hash of that directory's CONTENT: it
/// changes if and only if the crate changes. Measured on this repo — the same
/// hash across five consecutive commits that touched other things.
///
/// **Why not `CARGO_PKG_VERSION`.** It moves at RELEASE cadence (8 bumps in the
/// seven weeks to 2026-09-18; this repo forbids version bumps in feature PRs),
/// so hundreds of commits land between changes. Measured: a tree at
/// `beta.163` with an installed extension reporting `beta.163` that was stale
/// enough to need rebuilding twice in one day. A version check would have been
/// green for both, which is worse than no check because it would be believed.
///
/// **Degrades to empty, never fails the build.** No git (an sdist, a release
/// tarball, a build container without the history) means no stamp, and the
/// check treats an unstamped extension as "cannot tell" and stays quiet.
fn stamp_source_tree() {
    // Rebuild the stamp when the committed content could have moved. Without
    // this, cargo caches build.rs output and the stamp goes stale on its own —
    // which would be a staleness checker that is itself stale.
    println!("cargo:rerun-if-changed=../secantus-core/src");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    let trees: Vec<String> = ["crates/secantus-core", "crates/secantus-core-py"]
        .iter()
        .filter_map(|path| {
            std::process::Command::new("git")
                .args(["rev-parse", &format!("HEAD:{path}")])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .collect();
    let stamp = if trees.len() == 2 { trees.join("-") } else { String::new() };
    println!("cargo:rustc-env=SECANTUS_SOURCE_TREE={stamp}");
}

fn main() {
    stamp_source_tree();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    }
}
