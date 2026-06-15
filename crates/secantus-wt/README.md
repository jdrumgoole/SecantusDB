# secantus-wt

Safe Rust bindings over the vendored **WiredTiger** C library — the storage
foundation for SecantusDB's Rust engine (Phase 4 of the Python→Rust rewrite).

This crate wraps the slice of the WiredTiger C API that `secantus.storage` uses:
`Connection` / `Session` / `Cursor` lifecycle, the key formats of SecantusDB's
tables (`SS`, `SSu`, `SSS`, `SSSu`, `q`, `S`, `u`), error-code translation
(`WT_NOTFOUND` / `WT_DUPLICATE_KEY` / `WT_ROLLBACK`), and transactions. Key/value
packing is delegated to WiredTiger's own `set_key`/`get_key` (so it packs in C,
not in Python the way the old SWIG path did).

It is **not** a workspace member of `crates/Cargo.toml` (it links a native C
library that the pure `secantus-core` crates and their CI don't), so it builds
and tests standalone.

## Why separate from `secantus-core`

`secantus-core` is pure Rust (no native deps) and ships in the `secantus-core`
wheel across the whole manylinux/musllinux/macOS/Windows matrix. `secantus-wt`
links `libwiredtiger`, so folding it into that wheel matrix is the explicit
go/no-go gate for Phase 4 (see `tasks/rust-rewrite-phase4-scoping.md`). Until
that's solved it stays a standalone crate, exercised on machines/CI that have
WiredTiger built.

## Building / testing

`build.rs` resolves WiredTiger in this order:

1. `SECANTUS_WT_INCLUDE` (dir with `wiredtiger.h`) + `SECANTUS_WT_LIB` (dir with
   `libwiredtiger.a`/`.so`) — explicit override.
2. Probed build outputs: the project's CMake `build/*/wt-build`, and the dev
   sandbox's `/tmp/wt-build`.

bindgen needs libclang; set `LIBCLANG_PATH` if it isn't auto-discovered:

```bash
# from this directory
SECANTUS_WT_INCLUDE=/path/to/wt/include \
SECANTUS_WT_LIB=/path/to/wt/lib \
LIBCLANG_PATH=/usr/lib/llvm-18/lib \
cargo test
```

The integration tests in `tests/roundtrip.rs` run against a real on-disk
WiredTiger database (insert / sorted scan / point search / update / remove /
transactions / reopen-persistence).

## Status

Foundation only: the FFI layer + safe wrappers + tests are in place and green.
The `Storage` layer on top (the `secantus_collections` / `secantus_documents` /
index / oplog tables, the planner, the lock discipline) is the next slice — see
the scoping doc.
