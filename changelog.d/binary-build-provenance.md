### `--version` says which source a binary was built from

`secantusd-rs --version` and `secantusd-pg --version` reported only a crate
version — and this repo forbids version bumps in feature PRs, so hundreds of
commits share one string. A user reporting "0.1.0-beta.0" had told you almost
nothing about which code they were running.

#### Added

- Both binaries now print the git tree hash of the `crates/` sources they were
  built from, on a second line:

  ```
  $ secantusd-pg --version
  secantusd-pg 0.1.0-beta.0
  tree: 30bfc40839ab7200307814e7b38a5267d6f73bbd
  ```

  Line 1 stays short for the common "what did I install?"; line 2 is what makes
  a bug report actionable. The `tree:` line is **omitted, not blank**, when the
  binary was built without git (an sdist, a release tarball).
- The binary smoke tests compare that stamp against `HEAD:crates` and **fail**
  when `SECANTUSDB_BIN` / `SECANTUSD_PG_BIN` is set — the variable the release
  workflow uses to say "smoke THIS artifact", which is exactly when a stale
  binary would ship. A locally discovered binary only reports, because failing
  someone mid-edit is how a check gets switched off.

  This extends the `_secantus_core` provenance check to the two distributed
  artifacts, which accounted for the majority of the day's staleness incidents.

#### Fixed

- `secantusd-rs --version` printed without a trailing newline, so its output ran
  into the next shell prompt.

A **tree** hash, not the commit SHA: `HEAD:crates` changes only when a crate's
content changes — measured stable across three consecutive commits touching
only docs and tests — whereas a commit SHA moves constantly and a check built on
it would be disabled within a week.
