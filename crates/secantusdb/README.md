# secantus-mdb

A **single-node MongoDB server**: it speaks the real MongoDB wire protocol on
a real TCP socket, over the same WiredTiger storage engine MongoDB ships. Any
MongoDB driver connects to it as it would to `mongod`.

This package provides the `secantusd-mdb` binary. Building it compiles
WiredTiger from bundled source, which needs **CMake and a C compiler**; nothing
else (no Python, no libclang). The first build takes a minute or so.

```sh
cargo install secantus-mdb --version 0.5.3-beta.199   # a pre-release: cargo needs the version
secantusd-mdb --port 27018
```

Or skip the build: `pip install SecantusDB` installs the prebuilt
`secantusd-mdb` (and `secantusd-pg`) on your `PATH` (Linux x86_64, macOS
arm64), and prebuilt binaries for each platform are on the
[GitHub releases](https://github.com/jdrumgoole/SecantusDB/releases).

Project home and documentation: <https://secantusdb.com>.

## Licence

GPL-2.0-only. WiredTiger, which this links statically, is GPL v2 or v3.
