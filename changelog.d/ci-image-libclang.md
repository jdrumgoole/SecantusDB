### CI: bindgen uses the runner image's libclang, not an apt download

The `apt` install of `libclang-18-dev` (with `clang` and `llvm-dev`) stalled
for 4 to 5 minutes on the Azure Ubuntu mirror in about half the runs of the
jobs that build the Rust servers, and whichever job it hit set the PR's CI
time. The ubuntu-24.04 runner image already ships clang 16, 17 and 18, each
with its shared libclang and the clang resource headers bindgen reads.

#### Added

- `.github/actions/image-libclang`: sets `LIBCLANG_PATH` to the newest
  `/usr/lib/llvm-*/lib` that has both libclang and its resource headers, and
  fails the job if none does.

#### Changed

- `test.yml` (`rust-storage`, `storage-engine` on Linux, `pg-oracle`) and
  `psycopg-tls.yml` no longer install `clang`, `libclang-dev` or `llvm-dev`;
  they call the action instead, and the `find` that located libclang in
  `pg-oracle` and `psycopg-tls` is gone. The release workflows are unchanged.
- `.github/actions/apt-install`: `apt-get update` has its own deadline,
  `update-timeout` (default 90s), instead of sharing the install's 240s. On
  this PR's first run the Azure mirror stalled the update of `storage-engine`,
  and the job waited out the full 240s before the retry that pins
  `archive.ubuntu.com`. A timeout now names the phase that stalled.
