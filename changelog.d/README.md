# Changelog fragments

**Do not edit `docs/changelog.md` in a feature PR.** Add a fragment here instead.

Every PR that ships a user-visible change adds one file to this directory:

```
changelog.d/<short-slug>.md
```

The slug should be descriptive and unlikely to collide (e.g.
`push-addtoset-skip-missing.md`, `sql-having-grouping-sets.md`). One entry per
file.

## Why

When multiple sessions each edit the top of `docs/changelog.md`'s
`## [Unreleased]` section, every pair of concurrent PRs conflicts on that one
file — merging one forces the others to rebase and re-resolve the changelog by
hand. New files never conflict, so fragments make concurrent PRs independent.
(The same reasoning is why feature PRs no longer bump the `version` line —
see the project `CLAUDE.md`.)

## Format

A fragment is the changelog entry exactly as it would appear inline: a `###`
headline (which becomes the release blog-post title), a 1–3 paragraph prose
lede, then the engineering `####` sections. Example:

```markdown
### Short headline in sentence case

One to three paragraphs of self-contained prose describing the change and why it
matters — this is lifted verbatim as the blog-post body, so it should read as
narrative, not as "vX.Y.Z ships X".

#### Fixed

- `module.py` / `crate`: what changed, concisely.
```

Use `#### Added` / `#### Changed` / `#### Fixed` / `#### Deprecated` /
`#### Removed` / `#### Security` as appropriate. Do **not** include a `## [X.Y.Z]`
or `## [Unreleased]` header or a version number — those are added at release time.

## Release

At release, `invoke changelog-collate` folds every fragment into the
`## [Unreleased]` section of `docs/changelog.md` (in filename order) and deletes
the fragments. The normal promote-`[Unreleased]`-to-a-dated-section step then
proceeds as before. `release-prepare` runs this automatically.
