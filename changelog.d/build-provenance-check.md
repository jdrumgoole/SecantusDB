### A stale build no longer looks like a regression

On 2026-09-18 eight separate incidents came from a built artifact being older
than the tree it was tested against. Every one presented as somebody else's
regression, because the tests were current and only the artifact was old: a
conformance gauge reported 1,084 failures and a 73.8% pass rate from a binary
50 crate-commits behind, and that wrong number reached the public website; a
full suite produced 174 parity failures that were all fictional, the same files
passing 207 of 207 after a rebuild with no code change; and three failures
landed in a colleague's new test file, reading as their bug, from a binary
built 33 minutes before the fix those tests assert.

Reading the diff cannot catch this, because the evidence is not in the diff.
The suite now checks at collection whether the installed `_secantus_core` was
built from the sources in the checkout, and stops with the rebuild command if
it was not.

The identifier is the git TREE hash of the crates, not the commit SHA and not
the package version. The commit SHA moves on every commit, so that check would
report stale constantly and be switched off; the version moves at release
cadence, and was measured reporting `beta.163` for an extension stale enough to
need rebuilding twice in a day — green for exactly the builds that were wrong.
A tree hash changes if and only if the crate's content changes.

#### Added
- `secantus-core-py`: `build.rs` stamps the source tree hash, exposed as
  `_secantus_core.__source_tree__`. Absent without git history (an sdist, a
  build container), in which case the check abstains.
- `tests/conftest.py`: the collection-time check, and `tests/test_build_provenance.py`
  pinning its decisions — weighted toward the cases where it must stay SILENT,
  since a false positive is how a check gets disabled.
