### CI broke on a crate nobody here depends on, because two lockfiles were ignored

`tinyvec 1.13.0` shipped a library that does not compile — `TinyVec::Heap(vec![
...])` with `vec!` not in scope — and took the whole of CI with it on a commit
that changed no dependency.

`test.yml`'s `rust-storage` job runs `cargo fmt / clippy / test` in each of six
WT-linked crate directories, which makes every one of them its own **dependency
resolution root**. Four of the six commit a `Cargo.lock` and were unaffected —
`secantusdb` among them, which pulls tinyvec and pins 1.11.0. The two whose
locks were gitignored re-resolved to 1.13.0 and failed, and so did the
storage-engine wheel build on both Linux and Windows.

The exclusion had a comment justifying it: these crates are "built only by CI's
CMake path, so a stray per-crate `target/` + `Cargo.lock` should never be
committed". The first half is not true — the workflow builds them directly —
and the second conflated a build artifact with a lockfile. The `target/`
directories stay ignored; the two locks are now tracked, which makes the set of
six consistent.

Nothing about this was specific to one branch: `main` would have failed on its
next run.

#### Fixed

- `.gitignore`: `crates/secantus-storage-adapter/Cargo.lock` and
  `crates/secantus-server-py/Cargo.lock` are no longer ignored, with the reason
  recorded where the old claim used to be.
- Both lockfiles committed, pinning `tinyvec 1.12.0`.
