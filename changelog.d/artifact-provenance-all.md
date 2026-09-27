### A stale build was only caught for one of four artifacts

#1489 added a collection-time check that refuses to run when the installed
`_secantus_core` was built from different sources than the checkout. It covered
that one extension. `_secantus_server`, `secantusd-pg` and `secantusd-rs` had no
such protection, and on 2026-09-27 that cost **nine false-regression diagnoses in
a single session**:

- a stale `_secantus_server` — six days behind, predating #1569 — failed a
  colleague's entire new failpoint test file with 3 failures and a **hang** that
  killed three xdist workers at 99%, surfacing as a bare `rc=70` with no summary
  line. It was diagnosed as "main is broken" and reported as such. 13 of 13
  passed after a rebuild, with no code change.
- a stale `secantusd-pg` produced failures on **six** separate occasions, each
  time in the test file whose fix the binary predated, each time reading as
  somebody else's regression.

The binaries already **carried** a source stamp from #1492 and nothing read it —
the worst arrangement, with the evidence sitting in `--version` output that no
automated thing looked at.

#### Added

- `secantus-server-py`: `build.rs` stamps the crates tree hash, exposed as
  `_secantus_server.__source_tree__`, mirroring `secantus-core-py`.
- `tests/conftest.py`: the collection-time check now covers all four artifacts,
  reading the binaries' stamps via `--version`.
- `tests/test_build_provenance.py`: cases for the generic check, weighted toward
  the configurations where it must stay **silent** — an unstamped artifact, a
  checkout git cannot read, a binary that is not built — because a false
  positive is how a check gets disabled.

#### Notes on the design

The identifier is a git **tree** hash, not the commit SHA (which moves on every
commit and would cry stale constantly) and not the package version (constant for
these path dependencies — #1489 measured `beta.163` reported by a build stale
enough to need rebuilding twice in a day). This is why no Python packaging tool
solves it: pip and uv compare versions, and `uv sync` reuses a cached build for a
path dependency whose version has not moved.

`_secantus_core` keeps its narrower two-crate stamp so an unrelated crate's
change does not read as stale there; the server extension links most of the
workspace and takes the whole `crates` tree, which is also what both binaries
stamp. Conflating them would either over-report on core or under-report on the
server.

The rebuild commands are asserted against `rust_tasks.py` rather than trusted:
the first draft pointed `secantusd-rs` at `rust-server-build`, which rebuilds the
embedded extension instead of the binary. A check that prints the wrong remedy
sends the reader on a detour and teaches them to distrust it.
