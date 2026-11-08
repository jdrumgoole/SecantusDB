### pg-oracle caches its cargo build; Rust caches save from main only

#### Changed

- The `pg-oracle` job had no cargo cache. Each of its three shards compiled
  every third-party crate on every run: "Build secantusd-pg" took
  108-153 s, "Build secantusd-rs" 65 s, and the build accounted for most of
  the cargo tests' 99 s. This is the job that sets a PR's CI time.
  - A `Swatinem/rust-cache` step now covers `crates/secantus-pg` and
    `crates/secantusdb`, with one key shared by the three shards.
  - Only the `server` shard builds all of it, so only that shard saves the
    cache, and only on `main`.
- The `rust` and `rust-storage` cargo caches now save from `main` only.
  - A cache a PR saves can be read by that PR alone, and PRs already
    restore main's cache.
  - On 2026-11-08 the PR-scoped copies held 3.2 GB of the repo's 10 GB
    Actions cache, so the caches every job reads were being evicted.
