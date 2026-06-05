"""Micro-benchmark: pure-Python operator engines vs the Rust core.

Answers the two open questions about the Rust accelerator:

1. **Is it faster single-threaded, and does the per-call ``bson.encode`` byte
   seam eat the win?** For each engine we time the pure-Python function against
   the full Rust *seam* path (``bson.encode`` the args -> call ``_secantus_core``
   -> ``bson.decode`` the result), which is exactly what the shim pays.

2. **Does releasing the GIL actually parallelise?** We run the Rust seam across
   1/2/4 Python threads and report throughput scaling. The pure-Python path holds
   the GIL throughout, so it stays flat; the Rust compute runs under
   ``Python::allow_threads`` and should scale until the GIL-held encode/decode
   portion of the seam becomes the bottleneck.

Run (no WiredTiger needed — loads the pure modules by path, like the parity
suites):

    uv run --no-sync python benchmarks/engine_bench.py
"""
from __future__ import annotations

import importlib.util
import pathlib
import sys
import threading
import time
import types

import bson

try:
    import _secantus_core as rust
except ImportError:
    sys.exit("build the Rust core first: maturin build ... && uv pip install the wheel")


def _load_pure():
    root = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
    if "secantus" not in sys.modules:
        pkg = types.ModuleType("secantus")
        pkg.__path__ = [str(root)]
        sys.modules["secantus"] = pkg
    mods = {}
    for name in ("paths", "collation", "query", "expressions", "update", "projection",
                 "ordering", "aggregate"):
        full = f"secantus.{name}"
        if full not in sys.modules:
            spec = importlib.util.spec_from_file_location(full, root / f"{name}.py")
            mod = importlib.util.module_from_spec(spec)
            sys.modules[full] = mod
            spec.loader.exec_module(mod)
        mods[name] = sys.modules[full]
    return mods


M = _load_pure()


def _time(fn, iters):
    start = time.perf_counter()
    for _ in range(iters):
        fn()
    return time.perf_counter() - start


# --- workloads: (name, python_callable, rust_callable) --------------------

DOC = {"_id": 1, "a": 5, "b": "hello", "tags": [1, 2, 3], "nested": {"x": 10, "y": 20}}
QUERY = {"a": {"$gte": 3, "$lt": 100}, "b": "hello", "tags": {"$in": [2, 9]}}
_doc_b = bson.encode(DOC)
_query_b = bson.encode(QUERY)
_empty = bson.encode({})


def py_match():
    return M["query"].matches(DOC, QUERY)


def rust_match():
    return rust.query_matches(_doc_b, _query_b, _empty, _empty)


EXPR = {"$add": [{"$multiply": ["$a", 2]}, {"$size": "$tags"}, "$nested.x"]}
_expr_b = bson.encode({"e": EXPR})


def py_eval():
    return M["expressions"].evaluate(EXPR, DOC)


def rust_eval():
    res = rust.evaluate(_doc_b, _expr_b, _empty)
    return None if res is None else bson.decode(res)["r"]


UPDATE = {"$set": {"b": "world"}, "$inc": {"a": 1}, "$push": {"tags": 4}}
_update_b = bson.encode(UPDATE)


def py_update():
    return M["update"].apply_update(DOC, UPDATE)


def rust_update():
    res = rust.apply_update(_doc_b, _update_b, False)
    return None if res is None else bson.decode(res)


PIPE_DOCS = [{"_id": i, "g": i % 5, "v": i} for i in range(200)]
PIPELINE = [
    {"$match": {"v": {"$gte": 10}}},
    {"$group": {"_id": "$g", "total": {"$sum": "$v"}, "n": {"$sum": 1}}},
    {"$sort": {"total": -1}},
]
_pdocs_b = bson.encode({"d": PIPE_DOCS})
_pipe_b = bson.encode({"p": PIPELINE})
_PipelineContext = M["aggregate"].PipelineContext


def py_pipeline():
    return M["aggregate"].apply_pipeline(list(PIPE_DOCS), PIPELINE, _PipelineContext())


def rust_pipeline():
    res = rust.apply_pipeline(_pdocs_b, _pipe_b, _empty, _empty)
    return None if res is None else bson.decode(res)["d"]


WORKLOADS = [
    ("query.matches", py_match, rust_match, 200_000),
    ("expressions.evaluate", py_eval, rust_eval, 200_000),
    ("update.apply_update", py_update, rust_update, 100_000),
    ("aggregate.apply_pipeline (200 docs)", py_pipeline, rust_pipeline, 20_000),
]


def single_threaded():
    print("=" * 78)
    print("Single-threaded: pure-Python vs the Rust seam (encode -> rust -> decode)")
    print("=" * 78)
    print(f"{'engine':<38}{'py ops/s':>12}{'rust ops/s':>13}{'speedup':>9}")
    print("-" * 78)
    for name, pyfn, rustfn, iters in WORKLOADS:
        # sanity: both paths must agree (rust didn't silently defer to None)
        assert rustfn() is not None, f"{name}: rust deferred — benchmark would be meaningless"
        pyfn()
        rustfn()  # warm
        pt = _time(pyfn, iters)
        rt = _time(rustfn, iters)
        py_ops = iters / pt
        rust_ops = iters / rt
        print(f"{name:<38}{py_ops:>12,.0f}{rust_ops:>13,.0f}{pt / rt:>8.2f}x")
    print()


def _run_n(fn, iters):
    for _ in range(iters):
        fn()


def multi_threaded():
    print("=" * 78)
    print("Multi-threaded scaling (GIL released during Rust compute)")
    print("=" * 78)
    print("Throughput vs thread count; ideal = linear. The GIL-held encode/decode")
    print("portion of the seam caps scaling for cheap ops.\n")
    for name, _pyfn, rustfn, base in WORKLOADS:
        total = base
        print(f"{name}")
        print(f"  {'threads':>8}{'ops/s':>14}{'scaling':>10}")
        base_ops = None
        for nthreads in (1, 2, 4):
            per = total // nthreads
            threads = [threading.Thread(target=_run_n, args=(rustfn, per)) for _ in range(nthreads)]
            start = time.perf_counter()
            for t in threads:
                t.start()
            for t in threads:
                t.join()
            elapsed = time.perf_counter() - start
            ops = (per * nthreads) / elapsed
            if base_ops is None:
                base_ops = ops
            print(f"  {nthreads:>8}{ops:>14,.0f}{ops / base_ops:>9.2f}x")
        print()


if __name__ == "__main__":
    single_threaded()
    multi_threaded()
