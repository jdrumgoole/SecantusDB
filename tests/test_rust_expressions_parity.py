"""Parity: Rust `_secantus_core.evaluate` vs pure-Python `expressions.evaluate`.

Phase 1 net for the fourth (largest) leaf engine. For each (expr, doc) the Rust
evaluator runs over BSON bytes; when it returns a concrete result (didn't
defer), the decoded value must equal the authoritative pure-Python `evaluate`.
When it returns None (whole-call fallback — any operator/value not ported)
there's nothing to assert.

Import-light: prefers the real `secantus.expressions`, else loads
`expressions.py` + `paths.py` by path under a stub `secantus` package. The
corpus sticks to the ported core (paths, comparison, logic, control flow,
arithmetic, common array ops) so the pure path never needs the lazily-imported
`secantus.storage` / `bson.Regex` paths.
"""
from __future__ import annotations

import datetime
import importlib.util
import pathlib
import random
import sys
import types

import bson
import pytest

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")


def _load_pure_expr():
    try:
        from secantus import expressions as e

        return e
    except Exception:
        pass
    root = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
    if "secantus" not in sys.modules:
        pkg = types.ModuleType("secantus")
        pkg.__path__ = [str(root)]
        sys.modules["secantus"] = pkg
    for name in ("paths", "expressions"):
        full = f"secantus.{name}"
        if full not in sys.modules:
            spec = importlib.util.spec_from_file_location(full, root / f"{name}.py")
            mod = importlib.util.module_from_spec(spec)
            sys.modules[full] = mod
            spec.loader.exec_module(mod)
    return sys.modules["secantus.expressions"]


_pure = _load_pure_expr()


def _rust_eval(expr, doc, vars=None):
    res = _rust.evaluate(
        bson.encode(doc), bson.encode({"e": expr}), bson.encode(vars or {})
    )
    return None if res is None else bson.decode(res)["r"]


_DT = datetime.datetime(2026, 6, 5, 12, 34, 56, tzinfo=datetime.timezone.utc)

# (expr, doc) pairs over the ported operator core.
CURATED = [
    ("$a", {"a": 5}),
    ("hi", {}),
    ("$missing", {}),
    ("$a.b", {"a": {"b": 7}}),
    ({"$literal": "$notapath"}, {}),
    ({"$eq": ["$a", "$b"]}, {"a": 5, "b": 5}),
    ({"$eq": [1, 1.0]}, {}),
    ({"$eq": [True, 1]}, {}),
    ({"$ne": [1, 2]}, {}),
    ({"$gt": ["$a", "$b"]}, {"a": 5, "b": 3}),
    ({"$gt": ["$a", "$b"]}, {"a": 1, "b": 3}),
    ({"$gte": [5, 5]}, {}),
    ({"$lt": ["apple", "banana"]}, {}),
    ({"$lte": [3, 2]}, {}),
    ({"$gt": [1, "x"]}, {}),  # cross-type -> False
    ({"$and": [True, {"$eq": [1, 1]}]}, {}),
    ({"$or": [False, {"$gt": [2, 5]}]}, {}),
    ({"$not": [{"$eq": [1, 2]}]}, {}),
    ({"$cond": [{"$gt": ["$a", 0]}, "pos", "neg"]}, {"a": 5}),
    ({"$cond": {"if": {"$gt": ["$a", 0]}, "then": "pos", "else": "neg"}}, {"a": -1}),
    ({"$ifNull": ["$x", 7]}, {}),
    ({"$ifNull": ["$a", 7]}, {"a": 3}),
    ({"$switch": {"branches": [{"case": {"$gt": ["$a", 10]}, "then": "big"}],
                  "default": "small"}}, {"a": 5}),
    ({"$add": [1, 2, 3]}, {}),
    ({"$add": ["$a", 1]}, {}),  # missing -> null
    ({"$add": [1.5, 2]}, {}),
    ({"$add": [5]}, {}),
    ({"$subtract": [10, 3]}, {}),
    ({"$multiply": [2, 3, 4]}, {}),
    ({"$multiply": [2, 2.5]}, {}),
    ({"$divide": [9, 2]}, {}),
    ({"$divide": [6, 0]}, {}),  # /0 -> null
    ({"$mod": [7, 3]}, {}),
    ({"$mod": [-7, 3]}, {}),
    ({"$size": "$a"}, {"a": [1, 2, 3]}),
    ({"$arrayElemAt": ["$a", -1]}, {"a": [10, 20, 30]}),
    ({"$first": "$a"}, {"a": [1, 2]}),
    ({"$last": "$a"}, {"a": [1, 2]}),
    ({"$concatArrays": [[1, 2], [3]]}, {}),
    ({"$reverseArray": [1, 2, 3]}, {}),
    ({"$in": [2, [1, 2, 3]]}, {}),
    ({"$in": [9, [1, 2, 3]]}, {}),
    # Nested
    ({"$cond": [{"$lt": [{"$add": ["$p", "$q"]}, 100]}, "cheap", "dear"]}, {"p": 30, "q": 40}),
    # String / array / object ops (now handled).
    ({"$concat": ["a", "b", "c"]}, {}),
    ({"$concat": ["x", "$a", "y"]}, {"a": "MID"}),
    ({"$concat": ["a", None, "b"]}, {}),  # None -> ""
    ({"$toUpper": "$a"}, {"a": "hi"}),
    ({"$toLower": "HELLO"}, {}),
    ({"$toUpper": "$a"}, {"a": 123}),  # non-string passes through
    ({"$strLenCP": "hello"}, {}),
    ({"$split": ["a,b,c", ","]}, {}),
    ({"$split": ["$a", "-"]}, {"a": "1-2-3"}),
    ({"$substrCP": ["hello", 1, 3]}, {}),
    ({"$substrCP": ["hello", 2, -1]}, {}),  # negative length -> to end
    ({"$substrCP": ["hello", 10, 2]}, {}),  # start past end -> ""
    ({"$slice": [[1, 2, 3, 4], 2]}, {}),
    ({"$slice": [[1, 2, 3, 4], -2]}, {}),
    ({"$slice": [[1, 2, 3, 4, 5], 1, 2]}, {}),
    ({"$slice": [[1, 2, 3, 4, 5], -3, 2]}, {}),
    ({"$indexOfArray": [[1, 2, 3, 2], 2]}, {}),
    ({"$indexOfArray": [[1, 2, 3, 2], 2, 2]}, {}),
    ({"$indexOfArray": [[1, 2, 3], 9]}, {}),
    ({"$mergeObjects": [{"a": 1}, {"b": 2}, {"a": 9}]}, {}),
    ({"$mergeObjects": [{"a": 1}, None, {"b": 2}]}, {}),
    ({"$objectToArray": "$o"}, {"o": {"x": 1, "y": 2}}),
    # Scope-introducing ops.
    ({"$map": {"input": [1, 2, 3], "in": {"$add": ["$$this", 10]}}}, {}),
    ({"$map": {"input": "$xs", "as": "n", "in": {"$multiply": ["$$n", 2]}}}, {"xs": [1, 2, 3]}),
    ({"$filter": {"input": [1, 2, 3, 4], "as": "n", "cond": {"$gt": ["$$n", 2]}}}, {}),
    ({"$filter": {"input": [1, 2, 3, 4, 5], "cond": {"$lt": ["$$this", 5]}, "limit": 2}}, {}),
    ({"$reduce": {"input": [1, 2, 3, 4], "initialValue": 0,
                  "in": {"$add": ["$$value", "$$this"]}}}, {}),
    ({"$reduce": {"input": "$xs", "initialValue": "", "in": {"$concat": ["$$value", "$$this"]}}},
     {"xs": ["a", "b", "c"]}),
    ({"$let": {"vars": {"d": {"$add": ["$x", 1]}}, "in": {"$multiply": ["$$d", 2]}}}, {"x": 5}),
    # $map referencing a ROOT field path inside `in` ($$CURRENT stays ROOT).
    ({"$map": {"input": [1, 2], "in": {"$add": ["$$this", "$base"]}}}, {"base": 100}),
    # Date component extractors.
    ({"$year": "$d"}, {"d": _DT}),
    ({"$month": "$d"}, {"d": _DT}),
    ({"$dayOfMonth": "$d"}, {"d": _DT}),
    ({"$hour": "$d"}, {"d": _DT}),
    ({"$dayOfWeek": "$d"}, {"d": _DT}),
    ({"$year": "$d"}, {"d": "not a date"}),  # non-date -> null
    # Cases that should defer (rust None -> skipped):
    ({"$toUpper": "café"}, {}),  # non-ASCII
    ({"$dateToString": {"date": "$d", "format": "%Y"}}, {"d": "x"}),
]


@pytest.mark.parametrize("expr,doc", CURATED)
def test_curated_parity(expr, doc):
    doc = bson.decode(bson.encode(doc))
    rust = _rust_eval(expr, doc)
    if rust is None:
        return
    py = _pure.evaluate(expr, doc)
    assert rust == py, f"rust={rust!r} pure={py!r} expr={expr}"


def _rand_scalar(rng):
    return rng.choice(
        [rng.randint(-20, 20), round(rng.uniform(-9, 9), 2), "a", "bb", "z", True, False, None]
    )


def _rand_doc(rng):
    d = {}
    for f in ("a", "b", "c"):
        r = rng.random()
        if r < 0.2:
            continue
        elif r < 0.4:
            d[f] = [rng.randint(0, 9) for _ in range(rng.randint(0, 3))]
        else:
            d[f] = _rand_scalar(rng)
    return d


def _rand_operand(rng, depth):
    r = rng.random()
    if depth <= 0 or r < 0.5:
        return rng.choice([rng.randint(-9, 9), round(rng.uniform(-5, 5), 1), "$a", "$b", "$c",
                           "lit", True, False])
    return _rand_expr(rng, depth - 1)


def _rand_expr(rng, depth):
    op = rng.choice(
        ["$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$add", "$subtract",
         "$multiply", "$divide", "$mod", "$and", "$or", "$not", "$cond", "$ifNull"]
    )
    if op in ("$and", "$or"):
        return {op: [_rand_operand(rng, depth) for _ in range(rng.randint(1, 3))]}
    if op == "$not":
        return {op: [_rand_operand(rng, depth)]}
    if op == "$cond":
        return {op: [_rand_operand(rng, depth) for _ in range(3)]}
    if op in ("$add", "$multiply"):
        return {op: [_rand_operand(rng, depth) for _ in range(rng.randint(1, 3))]}
    # binary ops
    return {op: [_rand_operand(rng, depth), _rand_operand(rng, depth)]}


def test_index_math_fuzz():
    """Stress $slice / $substrCP / $indexOfArray index arithmetic (the riskiest
    part — negative indices, out-of-range, clamping) against pure Python."""
    rng = random.Random(0x51CE)
    handled = 0
    for _ in range(6000):
        arr = [rng.randint(0, 4) for _ in range(rng.randint(0, 6))]
        s = "".join(rng.choice("abcde") for _ in range(rng.randint(0, 6)))
        lo, hi = rng.randint(-8, 8), rng.randint(-8, 8)
        expr = rng.choice([
            {"$slice": [arr, lo]},
            {"$slice": [arr, lo, hi]},
            {"$substrCP": [s, lo, hi]},
            {"$indexOfArray": [arr, rng.randint(0, 4)]},
            {"$indexOfArray": [arr, rng.randint(0, 4), lo]},
            {"$indexOfArray": [arr, rng.randint(0, 4), lo, hi]},
        ])
        expr = bson.decode(bson.encode({"e": expr}))["e"]
        rust = _rust_eval(expr, {})
        if rust is None:
            continue
        handled += 1
        py = _pure.evaluate(expr, {})
        assert rust == py, f"divergence: rust={rust!r} pure={py!r} expr={expr}"
    assert handled > 2000, f"expected many handled cases, only {handled}"


def test_date_extractor_fuzz():
    """Validate civil-date arithmetic ($year/$month/$dayOfMonth/$hour/$minute/
    $second/$dayOfWeek) across a wide instant range — including pre-epoch
    (negative millis) — against Python's datetime."""
    rng = random.Random(0xDA7E)
    epoch = datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)
    ops = ["$year", "$month", "$dayOfMonth", "$hour", "$minute", "$second", "$dayOfWeek"]
    for _ in range(4000):
        # ~ year 1900 .. 2400 (spans negative/positive millis)
        ms = rng.randint(-2_200_000_000_000, 13_600_000_000_000)
        dt = epoch + datetime.timedelta(milliseconds=ms)
        doc = bson.decode(bson.encode({"d": dt}))
        for op in ops:
            expr = {op: "$d"}
            rust = _rust_eval(expr, doc)
            if rust is None:
                continue
            py = _pure.evaluate(expr, doc)
            assert rust == py, f"{op}: rust={rust} pure={py} ms={ms} dt={doc['d']}"


def test_randomised_fuzz_parity():
    rng = random.Random(0xE5DA)
    handled = 0
    for _ in range(8000):
        doc = bson.decode(bson.encode(_rand_doc(rng)))
        expr = bson.decode(bson.encode({"e": _rand_expr(rng, 3)}))["e"]
        rust = _rust_eval(expr, doc)
        if rust is None:
            continue
        try:
            py = _pure.evaluate(expr, doc)
        except Exception:
            # Rust produced a value where Python raises -> a real divergence.
            pytest.fail(f"rust={rust!r} but pure raised; expr={expr} doc={doc}")
        handled += 1
        assert rust == py, f"divergence: rust={rust!r} pure={py!r} expr={expr} doc={doc}"
    assert handled > 1000, f"expected many handled cases, only {handled}"
