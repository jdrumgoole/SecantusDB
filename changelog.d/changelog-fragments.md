### Changelog fragments and release-time version assignment

Concurrent development got much less painful. Previously every PR edited the top
of `docs/changelog.md`'s `[Unreleased]` section and bumped the single `version`
line in `pyproject.toml` — two shared lines that made *any* two in-flight PRs
conflict, so merging one forced the others to rebase and hand-resolve the same
files. Feature PRs now add a `changelog.d/<slug>.md` fragment (one entry per
file) instead of touching `docs/changelog.md`, and they no longer bump the
Python package version at all — the version is assigned once, at release time, by
`release-prepare`. New fragment files never collide, so parallel sessions stay
independent.

#### Added

- `changelog.d/` fragment convention (`changelog.d/README.md`), a
  `changelog.fragments` collator, and an `invoke changelog-collate` task that
  folds fragments into `## [Unreleased]`. `release-prepare` runs the collation
  automatically before it stamps the version.

#### Changed

- Feature PRs no longer bump the Python `version` / `__version__` (assigned at
  release) or edit `docs/changelog.md` directly. The Rust crate version is still
  bumped per-PR (its `buildInfo` traceability handle; rare same-session-only
  collisions). See the Versioning and Conventions sections of `CLAUDE.md`.
