### CI's MongoDB backlog check can run a gap test about the build tooling

RM-388's gap test checks which WiredTiger build `rust_tasks` links, so it imports `rust_tasks`, and `rust_tasks` imports `invoke`. CI's backlog step installs only the test's server-side dependencies, so the module failed to import. `tools/backlog.py verify` then reported the item as `BROKEN` and failed the `storage-engine` lane. The step now installs `invoke` at the version in `uv.lock`.

#### Fixed

- `.github/workflows/test.yml`: the Rust MongoDB server's backlog step installs `invoke==3.0.3`.
