# secantus-core

The optional **Rust acceleration core** for [SecantusDB](https://github.com/jdrumgoole/SecantusDB).

This package provides the `_secantus_core` compiled extension — a Rust
reimplementation of SecantusDB's pure operator engines (sort keys, the query
matcher, update operators, the aggregation-expression evaluator, projection,
the change-stream diff, and the storage-independent aggregation pipeline). These
engines power the **separate Rust server**; the pure-Python `secantus` server
never imports this extension. The Rust core is reproduced byte-for-byte against
the pure-Python engines (pinned by the `tests/test_rust_*_parity.py` suites in
the main repo).

## Install

You normally don't install this directly — install it as a SecantusDB extra:

```bash
pip install "secantus[rust]"
```

That pulls in `secantus` plus this matching `secantus-core` wheel. Versions are
kept in lockstep: `secantus[rust]` pins `secantus-core` to the exact SecantusDB
version it accelerates.

## How it's used

There is **no in-process engine switching**. The old `SECANTUS_ENGINE` /
`SecantusDBServer(engine=...)` selection was retired in favour of two separate
servers — the pure-Python `secantus` server and a self-contained Rust server.
The `secantus` Python package never imports this extension.

`secantus-core` exists as (1) the reusable Rust engine library behind the Rust
server and the standalone `secantusd-rs` binary, and (2) the **parity-test oracle**:
the `tests/test_rust_*_parity.py` suites import `_secantus_core` directly and
assert each Rust engine matches its pure-Python counterpart byte-for-byte.

## Build from source

```bash
maturin build --release   # produces an abi3 wheel (CPython 3.10+)
```

See `tasks/rust-rewrite-plan.md` and `benchmarks/RESULTS.md` in the main repo for
the design (the BSON "byte seam", graceful fallback, GIL release, batched seams)
and performance characterisation.
