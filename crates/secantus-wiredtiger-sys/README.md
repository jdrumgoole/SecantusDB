# secantus-wiredtiger-sys

Builds WiredTiger (mongodb-7.0.33 with SecantusDB's build patches) from bundled
source as a static library, with zlib and lz4 linked statically. Needs CMake
and a C compiler.

One of those patches changes behaviour, not the build: under
`transaction_sync=(method=dsync)`, commits that arrive while a log write is in
flight share the next write instead of each making its own. A commit still
returns only after its record is written. `method=fsync` is unpatched.

Licence: GPL-2.0-only, as WiredTiger.

**Internal to [SecantusDB](https://github.com/jdrumgoole/SecantusDB).** This crate is an
implementation detail of the SecantusDB Rust servers, published so they can be
built from crates.io. It carries **no semver promise**: any release may change
its API. Depend on `secantus-mdb` instead.
