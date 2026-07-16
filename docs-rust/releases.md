# Releases

The Rust server releases on its **own version line**, independent of the
PyPI package. A release is a `secantusdb-v<crate-version>` tag (e.g.
`secantusdb-v0.5.3-beta.147`), which triggers a workflow that:

1. asserts the tag matches the lockstep crate version (a fat-fingered tag
   cannot ship a wrong-versioned binary),
2. builds static-WiredTiger archives for `x86_64-unknown-linux-gnu` and
   `aarch64-apple-darwin`,
3. smoke-tests each archive — the binary is booted and a full `pymongo`
   CRUD round-trip runs against it,
4. attaches `tar.gz` + `.sha256` pairs to a GitHub **pre-release** named
   after the tag.

Find them on the
[binary-releases listing](https://github.com/jdrumgoole/SecantusDB/releases?q=secantusdb-v&expanded=true)
(the `secantusdb-v*` tags — they're GitHub pre-releases, so the bare
releases page leads with the PyPI package's source-only `v*` releases
instead).

## Version scheme

`0.MAJOR.PATCH-beta.N` (SemVer pre-release), carried in lockstep across
every crate in the workspace. Most releases increment `N`; a patch (or
minor/major) bump resets the beta label to `.0`. The Python package's
PEP 440 version (`0.X.YbN` on PyPI) is a separate number — the two servers
diverged at `0.5.2` and advance independently. A change that touches only
one server bumps only that server's version.

## Platform matrix

| Target | Status |
| --- | --- |
| `x86_64-unknown-linux-gnu` | ✅ prebuilt archive |
| `aarch64-apple-darwin` | ✅ prebuilt archive |
| Windows | ❌ no standalone binary (the MSVC WiredTiger build emits no static lib) — use the wheel-bundled `_secantus_server` handle |
| musl-static Linux, `aarch64` Linux | documented follow-ons |
