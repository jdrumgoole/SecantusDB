/// Stamp the git tree hash of the workspace sources this binary is built from.
///
/// These are DISTRIBUTED artifacts, so the stamp's first job is answering
/// "which source produced this binary?" in a bug report — `--version` alone
/// cannot, because the crate version moves at release cadence (8 bumps in the
/// seven weeks to 2026-09-18; this repo forbids version bumps in feature PRs),
/// so hundreds of commits share one version string. A user reporting
/// "0.1.0-beta.0" has told you almost nothing. The test-time staleness check
/// falls out of the same stamp for free.
///
/// **A TREE hash, not the commit SHA**, matching `secantus-core-py/build.rs`.
/// `git rev-parse HEAD` moves on every commit including the ones that never
/// touch Rust, so a commit-SHA check would cry stale constantly and get
/// switched off. `HEAD:crates` is the hash of that directory's CONTENT --
/// measured on this repo as stable across three consecutive commits that
/// touched only docs and tests, and moving the moment a crate changed.
///
/// **The whole `crates/` tree, not this crate alone.** Both binaries link most
/// of the workspace through deep transitive paths (`secantusdb` →
/// `secantus-server` → `secantus-commands` → `secantus-core`, and so on).
/// Enumerating that closure here would be a second dependency list to keep in
/// sync with Cargo.toml, and it would rot silently the first time someone adds
/// a dependency — the stamp would keep reporting "unchanged" for a binary that
/// had changed.
///
/// **Degrades to empty, never fails the build.** No git — an sdist, a release
/// tarball, a container without history — means no stamp, and every consumer
/// treats an unstamped binary as "cannot tell" rather than "stale".
fn stamp_source_tree() {
    // Without these, cargo caches build.rs output and the stamp goes stale on
    // its own -- a staleness marker that is itself stale.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

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
}
