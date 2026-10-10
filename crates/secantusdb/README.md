# secantus-mdb

A **surrogate MongoDB server** for tests: it speaks the real MongoDB wire
protocol on a real TCP socket, over the same WiredTiger storage engine MongoDB
ships, scoped to a single node. Point an application's test suite at it instead
of standing up a real `mongod`.

This package provides the `secantusd-rs` binary. Building it compiles
WiredTiger from bundled source, which needs **CMake and a C compiler**; nothing
else (no Python, no libclang). The first build takes a minute or so.

```sh
cargo install secantus-mdb --version 0.5.3-beta.177   # a pre-release: cargo needs the version
secantusd-rs --port 27018
```

Prebuilt binaries for each platform are on the
[GitHub releases](https://github.com/jdrumgoole/SecantusDB/releases).

Project home and documentation: <https://secantusdb.com>.

## Licence

GPL-2.0-only. WiredTiger, which this links statically, is GPL v2 or v3.
