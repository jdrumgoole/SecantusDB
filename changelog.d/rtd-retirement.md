### Docs move to secantusdb.com; Read the Docs carries a pointer banner

The documentation is now self-hosted: the main tree at
`secantusdb.com/docs/` and the Rust server's tree at
`secantusdb.com/docs/rust/`, both deployed atomically with every website
publish. The release pipeline drops its four Read the Docs legs
(`release-finalize` now waits only for the publish workflow and the PyPI
listing, and no longer requires `READTHEDOCS_TOKEN`); README, and the new
`[project.urls]` PyPI metadata, point at the self-hosted locations. The
readthedocs.io copies stay online but every page there now carries a
banner linking to the up-to-date docs (furo's announcement bar, enabled
only when `READTHEDOCS=True` is in the build environment, so the
self-hosted build never shows it).
