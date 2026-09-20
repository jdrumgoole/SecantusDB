### The standalone binaries need nothing but libc, and CI now proves it

The Linux archives linked `liblz4.so.1` and `libz.so.1` — WiredTiger builds
both compressors as builtin extensions, so `libwiredtiger_static` references
them — which meant "download, extract, run" quietly assumed the host already
had two libraries a slim container image often does not. Both are now linked
from their static archives on the binary release lanes, so `secantusd-rs` and
`secantusd-pg` depend on the glibc family and nothing else. The wheel lanes
deliberately keep the shared objects: auditwheel bundles those, and changing
what a published wheel contains is not a side effect worth taking.

The larger problem was that nothing in the pipeline could tell. A published
archive can be broken in a way the build, the tests and the smoke test all
miss — the macOS binaries of 2026-09-19 linked liblz4 by its absolute Homebrew
path and would not launch on a Mac without Homebrew, while every stage passed
because the runner *has* Homebrew. `otool -L` on the artifact was the only
thing that ever reported it, and nobody ran `otool -L`.

So the release workflows now run it themselves. Before an archive is packaged,
`.github/scripts/check_binary_deps.py` reads the binary's real dependency table
— Mach-O load commands, ELF `DT_NEEDED`, or the PE import directory — and fails
the release if anything falls outside a per-platform allowlist. Checked against
the four artifacts that matter: it fails the published macOS binary naming the
Homebrew dylib, fails the published Linux one naming both compressors, and
passes the Windows archive and the fixed macOS build.

#### Added
- `.github/scripts/check_binary_deps.py`, run by both binary release workflows
  before packaging. The allowlist is glibc-family on Linux, `/usr/lib` +
  `/System/Library` on macOS, and OS DLLs on Windows — so a `VCRUNTIME140`
  dependency would fail the release too.
- `SECANTUS_WT_STATIC_COMPRESSORS=1` in `secantus-wt`'s build script, linking
  zlib and lz4 from their static archives on Linux. Opt-in, set only by the
  binary release lanes.

#### Changed
- The Linux release lanes install `zlib1g-dev` alongside `liblz4-dev`, for the
  `.a` archives.
