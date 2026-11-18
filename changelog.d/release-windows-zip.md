### Windows releases attach the `.zip` they already build

#### Fixed

- `release-binaries.yml` built a Windows `.zip` (and its `.sha256`) for every
  `secantusdb-v*` release but never attached it, so no release carried one
  while the docs said they did. A Windows-only step now attaches both,
  starting with the first release after 0.5.3-beta.197.
- `docs-rust/installation.md`: the download example ran `./secantusd-mdb`,
  but the archive unpacks into `./secantusdb-<version>-<target>/`; the Linux
  and Windows examples now use that path, the current release, and the
  `.tar.gz` that exists today.
