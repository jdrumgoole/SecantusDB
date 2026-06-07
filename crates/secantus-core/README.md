# secantus-core

The optional **Rust acceleration core** for [SecantusDB](https://github.com/jdrumgoole/SecantusDB).

This package provides the `_secantus_core` compiled extension — a Rust
reimplementation of SecantusDB's pure operator engines (sort keys, the query
matcher, update operators, the aggregation-expression evaluator, projection,
the change-stream diff, and the storage-independent aggregation pipeline). It is
an **accelerator, not a replacement**: SecantusDB always ships and works with
its pure-Python engines, and the Rust core is reproduced byte-for-byte against
them (pinned by the `tests/test_rust_*_parity.py` suites in the main repo).

## Install

You normally don't install this directly — install it as a SecantusDB extra:

```bash
pip install "secantus[rust]"
```

That pulls in `secantus` plus this matching `secantus-core` wheel. Versions are
kept in lockstep: `secantus[rust]` pins `secantus-core` to the exact SecantusDB
version it accelerates.

## Enable

Selection is process-wide and the **default is still Python**. Opt in with:

```bash
export SECANTUS_ENGINE=rust    # or "auto" (rust if the extension is importable)
```

or in code:

```python
import secantus.engine as engine
engine.set_engine("rust")      # or pass SecantusDBServer(engine="rust")
```

`rust` transparently falls back to the pure-Python path for anything the Rust
core doesn't (yet) reproduce, so it is always correct. Per-component overrides
(`SECANTUS_RUST_QUERY=1`, etc.) exist for debugging/bisection.

## Build from source

```bash
maturin build --release   # produces an abi3 wheel (CPython 3.10+)
```

See `tasks/rust-rewrite-plan.md` and `benchmarks/RESULTS.md` in the main repo for
the design (the BSON "byte seam", graceful fallback, GIL release, batched seams)
and performance characterisation.
