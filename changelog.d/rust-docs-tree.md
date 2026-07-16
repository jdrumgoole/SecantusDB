### Docs: a dedicated Rust-server documentation tree

The Rust server gets its own Sphinx tree (`docs-rust/`, built with
`invoke docs-rust`): installation from the prebuilt `secantusdb-v*` binary
archives, the full `secantusd-rs` CLI-flag and `secantusd.toml` reference,
the embedded `RustServer` handle, security (SCRAM / X509 / RBAC / rustls
TLS), backup and point-in-time recovery via `secantusd-rs restore`, the
crates architecture, conformance numbers, and the binary release track.
The tree is pure Markdown (no autodoc — its version is read from the
lockstep crate version), so it builds in any bare worktree, and it deploys
to secantusdb.com alongside the main docs.
