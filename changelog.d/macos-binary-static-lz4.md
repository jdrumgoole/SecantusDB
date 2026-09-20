### The macOS binaries only ran on a machine that had Homebrew

Both published macOS archives — `secantusd-rs` and `secantusd-pg` — linked
liblz4 by its absolute Homebrew path, `/opt/homebrew/opt/lz4/lib/liblz4.1.dylib`.
On a Mac without Homebrew lz4 installed at exactly that path, neither binary
launches at all. Nothing in the build said so: the release runner has Homebrew
by definition, the smoke test passes there, and `otool -L` on the shipped
archive is the only thing that ever reported it.

The cause was one word. `secantus-wt`'s build script emitted
`cargo:rustc-link-lib=dylib=lz4`, and because Homebrew ships `liblz4.a` and
`liblz4.dylib` side by side in the same directory, `dylib=` meant the linker
took the dylib every time even though a static archive sat next to it. macOS
now picks the link kind where it already picks the search path, and prefers the
static archive wherever one exists.

The fix for the release lanes was already in the tree and only the wheel build
used it: `tools/build_lz4_macos.sh` builds a static-only liblz4 at the right
deployment target, which is why the published macOS **wheel** has never had this
problem. Both binary workflows now build lz4 the same way instead of
`brew install lz4`. The Linux archives are unaffected — they link `liblz4.so.1`
and `libz.so.1` through the normal loader path — and the Windows archive was
already self-contained.

#### Fixed
- macOS `secantusd-rs` and `secantusd-pg` archives are now self-contained: they
  link only `/usr/lib` and `/System/Library` system libraries, with liblz4
  statically linked. Verified with `otool -L` and a 3,000-document round trip
  through lz4-compressed tables.
- A plain developer `cargo build` on macOS now produces a portable binary too,
  rather than one pinned to the builder's Homebrew prefix.

#### Changed
- `release-binaries.yml` and `release-pg-binaries.yml` build lz4 from source via
  `tools/build_lz4_macos.sh` and set `CMAKE_PREFIX_PATH` +
  `SECANTUS_WT_EXTRA_LIBDIR`, matching what `wheels.yml` has always done.
