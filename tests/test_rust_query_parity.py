"""Parity: Rust `_secantus_core.query_matches` vs pure-Python `query.matches`.

Phase 1 net for the second ported leaf engine. For each (doc, query) the Rust
matcher is run over BSON bytes; when it returns a concrete bool (i.e. it didn't
defer), that bool must equal the authoritative pure-Python `matches`. When it
returns None (fallback — collation/$jsonSchema/geo/uncompilable-regex/…) there
is nothing to assert: the shim would run pure Python anyway. ($regex/$options
and bare BSON regex are now matched in Rust via the `regex` crate; only patterns
the crate can't compile defer.)

Import-light: prefers the real `secantus.query`, but if the package can't be
imported (no WiredTiger extension built, as in a spike environment) it loads
`query.py` + `collation.py` by path under a stub `secantus` package. The corpus
deliberately avoids `$expr`/geo so the pure path never needs the not-yet-loaded
`secantus.expressions` / `secantus.geo` modules.
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
from bson import Decimal128, Int64, ObjectId, Regex

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")


def _load_pure_query():
    try:  # real package (full install / CI with the WiredTiger extension)
        from secantus import query as q

        return q
    except Exception:
        pass
    root = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
    if "secantus" not in sys.modules:
        pkg = types.ModuleType("secantus")
        pkg.__path__ = [str(root)]
        sys.modules["secantus"] = pkg
    for name in ("collation", "query"):
        full = f"secantus.{name}"
        if full not in sys.modules:
            spec = importlib.util.spec_from_file_location(full, root / f"{name}.py")
            mod = importlib.util.module_from_spec(spec)
            sys.modules[full] = mod
            spec.loader.exec_module(mod)
    return sys.modules["secantus.query"]


_pure = _load_pure_query()


def _rust_match(doc, query, collation=None):
    return _rust.query_matches(
        bson.encode(doc), bson.encode(query), bson.encode({}), bson.encode(collation or {})
    )


_Collation = sys.modules["secantus.collation"].Collation


def _coll(strength=3, case_level=False, numeric_ordering=False):
    """(wire dict for Rust, Collation object for pure Python)."""
    wire = {"strength": strength, "caseLevel": case_level, "numericOrdering": numeric_ordering}
    obj = _Collation(strength=strength, case_level=case_level, numeric_ordering=numeric_ordering)
    return wire, obj


# (doc, query, strength, case_level, numeric_ordering)
COLLATION_CASES = [
    ({"n": "PING"}, {"n": "ping"}, 2, False, False),  # case-insensitive match
    ({"n": "ping"}, {"n": "ping"}, 3, False, False),  # strength 3 identity
    ({"n": "PING"}, {"n": "ping"}, 3, False, False),  # case-sensitive -> no match
    ({"n": "PING"}, {"n": "ping"}, 1, True, False),  # caseLevel keeps case
    ({"n": "apple"}, {"n": {"$gte": "APPLE"}}, 2, False, False),
    ({"n": "Banana"}, {"n": {"$lt": "cherry"}}, 2, False, False),
    ({"n": "B"}, {"n": {"$in": ["a", "b", "c"]}}, 2, False, False),
    ({"n": "B"}, {"n": {"$nin": ["a", "b", "c"]}}, 2, False, False),
    ({"n": "x", "m": 5}, {"n": "X", "m": {"$gt": 3}}, 2, False, False),
    ({"n": "café"}, {"n": "CAFÉ"}, 2, False, False),  # non-ASCII -> rust defers
    ({"n": "a10"}, {"n": {"$gt": "a2"}}, 3, False, True),  # numericOrdering -> defers
]


@pytest.mark.parametrize("doc,query,strength,cl,no", COLLATION_CASES)
def test_collation_parity(doc, query, strength, cl, no):
    doc = bson.decode(bson.encode(doc))
    query = bson.decode(bson.encode(query))
    wire, obj = _coll(strength, cl, no)
    rust = _rust_match(doc, query, wire)
    if rust is None:
        return  # rust deferred (non-ASCII / numericOrdering) -> shim uses Python
    py = _pure.matches(doc, query, collation=obj)
    assert rust == py, f"rust={rust} pure={py} q={query} strength={strength}"


def test_collation_fuzz():
    rng = random.Random(0xC0_11A)
    words = ["apple", "Apple", "APPLE", "banana", "BANANA", "Cherry", "date", "a1", "B"]
    for _ in range(4000):
        doc = {"n": rng.choice(words)}
        target = rng.choice(words)
        op = rng.choice(["eq", "$gt", "$gte", "$lt", "$lte", "$ne", "$in"])
        if op == "eq":
            query = {"n": target}
        elif op == "$in":
            query = {"n": {"$in": rng.sample(words, rng.randint(1, 3))}}
        else:
            query = {"n": {op: target}}
        wire, obj = _coll(strength=rng.choice([1, 2, 3]), case_level=rng.random() < 0.3)
        d = bson.decode(bson.encode(doc))
        q = bson.decode(bson.encode(query))
        rust = _rust_match(d, q, wire)
        if rust is None:
            continue
        assert rust == _pure.matches(d, q, collation=obj), f"q={q} wire={wire} doc={d}"


def _rt(value):
    return bson.decode(bson.encode({"v": value}))["v"]


# (doc, query) pairs mirroring tests/test_query.py for the operators the Rust
# matcher handles, plus numeric-bridge and bool-distinctness cases.
CURATED = [
    ({"a": 1}, {}),
    ({"a": 1}, {"a": 1}),
    ({"a": 1}, {"a": 2}),
    ({"a": {"b": {"c": 5}}}, {"a.b.c": 5}),
    ({"a": {"b": {"c": 5}}}, {"a.b.c": 6}),
    ({"tags": ["red", "blue", "green"]}, {"tags": "red"}),
    # Embedded-document equality is order-sensitive + exact (Rust defers
    # on Document/Array expected values; Python is the oracle).
    ({"s": {"h": 14, "w": 21}}, {"s": {"h": 14, "w": 21}}),
    ({"s": {"h": 14, "w": 21}}, {"s": {"w": 21, "h": 14}}),
    ({"s": {"h": 14, "w": 21}}, {"s": {"h": 14}}),
    ({"s": {"a": [1, 2], "b": 3}}, {"s": {"a": [1, 2], "b": 3}}),
    ({"s": {"a": [1, 2], "b": 3}}, {"s": {"a": [2, 1], "b": 3}}),
    ({"tags": ["red", "blue"]}, {"tags": "yellow"}),
    ({"items": [{"sku": "a"}, {"sku": "b"}]}, {"items.sku": "b"}),
    ({"items": [{"sku": "a"}, {"sku": "b"}]}, {"items.sku": "c"}),
    ({"vals": [10, 20, 30]}, {"vals.1": 20}),
    ({"vals": [10, 20, 30]}, {"vals.1": 10}),
    ({"age": 30}, {"age": {"$gt": 20}}),
    ({"age": 30}, {"age": {"$gte": 30}}),
    ({"age": 30}, {"age": {"$lt": 31}}),
    ({"age": 30}, {"age": {"$lt": 30}}),
    ({"a": 2}, {"a": {"$in": [1, 2, 3]}}),
    ({"a": 4}, {"a": {"$in": [1, 2, 3]}}),
    ({"a": 4}, {"a": {"$nin": [1, 2, 3]}}),
    ({"a": None}, {"a": {"$exists": True}}),
    ({}, {"a": {"$exists": False}}),
    ({}, {"a": {"$exists": True}}),
    ({}, {"a": None}),
    ({"a": None}, {"a": None}),
    ({"a": 1, "b": 2}, {"$and": [{"a": 1}, {"b": 2}]}),
    ({"a": 1, "b": 2}, {"$and": [{"a": 1}, {"b": 3}]}),
    ({"a": 1, "b": 2}, {"$or": [{"a": 99}, {"b": 2}]}),
    ({"a": 1, "b": 2}, {"$nor": [{"a": 99}, {"b": 99}]}),
    ({"a": 5}, {"a": {"$not": {"$gt": 10}}}),
    ({"a": 50}, {"a": {"$not": {"$gt": 10}}}),
    ({"a": "hi"}, {"a": {"$type": "string"}}),
    ({"a": 1}, {"a": {"$type": "string"}}),
    ({"a": 1.5}, {"a": {"$type": "double"}}),
    ({"a": 1}, {"a": {"$type": "number"}}),
    ({"a": Decimal128("1.5")}, {"a": {"$type": "number"}}),
    ({"a": "x"}, {"a": {"$type": "number"}}),
    ({"a": "hi"}, {"a": {"$type": ["string", "int"]}}),
    ({"a": 1.5}, {"a": {"$type": ["string", "int"]}}),
    ({"a": Int64(5)}, {"a": {"$type": "long"}}),
    ({"a": 5}, {"a": {"$type": "int"}}),
    ({"tags": [1, 2, 3]}, {"tags": {"$size": 3}}),
    ({"tags": [1, 2]}, {"tags": {"$size": 3}}),
    ({"tags": "abc"}, {"tags": {"$size": 3}}),
    ({"n": 12}, {"n": {"$mod": [4, 0]}}),
    ({"n": 13}, {"n": {"$mod": [4, 1]}}),
    ({"n": 13}, {"n": {"$mod": [4, 0]}}),
    ({"vals": [3, 7, 12]}, {"vals": {"$mod": [4, 0]}}),
    (
        {"items": [{"sku": "a", "qty": 1}, {"sku": "b", "qty": 5}]},
        {"items": {"$elemMatch": {"sku": "b", "qty": {"$gte": 5}}}},
    ),
    (
        {"items": [{"sku": "a", "qty": 1}, {"sku": "b", "qty": 5}]},
        {"items": {"$elemMatch": {"sku": "b", "qty": {"$gt": 5}}}},
    ),
    ({"vals": [1, 5, 10]}, {"vals": {"$elemMatch": {"$gte": 3, "$lt": 7}}}),
    ({"vals": [1, 5, 10]}, {"vals": {"$elemMatch": {"$gte": 11}}}),
    ({"a": 1}, {"a": 1, "$comment": "hi"}),
    ({"a": 2}, {"a": 1, "$comment": "hi"}),
    ({"flags": 0b1011}, {"flags": {"$bitsAllSet": 0b1010}}),
    ({"flags": 0b1001}, {"flags": {"$bitsAllSet": 0b1010}}),
    ({"flags": 0b1011}, {"flags": {"$bitsAllSet": [0, 1, 3]}}),
    ({"flags": 0b0010}, {"flags": {"$bitsAnySet": 0b1010}}),
    ({"flags": 0b0001}, {"flags": {"$bitsAllClear": 0b1010}}),
    ({"flags": 0b1010}, {"flags": {"$bitsAnyClear": 0b1011}}),
    ({"flags": "abc"}, {"flags": {"$bitsAllSet": 0b1}}),
    ({"flags": True}, {"flags": {"$bitsAllSet": 0b1}}),
    ({"x": Decimal128("5")}, {"x": 5}),
    ({"x": 5}, {"x": Decimal128("5")}),
    ({"x": Decimal128("3.5")}, {"x": 3.5}),
    ({"x": Decimal128("5")}, {"x": 6}),
    ({"x": [Decimal128("5"), Decimal128("6")]}, {"x": 6}),
    ({"x": Decimal128("5")}, {"x": {"$in": [3, 5, 7]}}),
    ({"x": True}, {"x": 1}),
    ({"x": 1}, {"x": True}),
    ({"x": False}, {"x": 0}),
    ({"x": Decimal128("3.5")}, {"x": {"$gt": 2}}),
    ({"x": 3.5}, {"x": {"$gt": Decimal128("2")}}),
    ({"x": Decimal128("1.5")}, {"x": {"$gt": 2}}),
    ({"x": Decimal128("3.5")}, {"x": {"$lte": 4.0}}),
    ({"x": "banana"}, {"x": {"$gt": "apple"}}),
    ({"x": "apple"}, {"x": {"$gte": "banana"}}),
    ({"d": datetime.datetime(2026, 1, 1)}, {"d": {"$lt": datetime.datetime(2027, 1, 1)}}),
    ({"a": 1}, {"a": {"$ne": 2}}),
    ({"a": 1}, {"a": {"$ne": 1}}),
    ({"a": None}, {"a": {"$ne": None}}),
    ({}, {"a": {"$ne": None}}),
    # $expr — now handled in Rust via the expression evaluator.
    ({"a": 5, "b": 3}, {"$expr": {"$gt": ["$a", "$b"]}}),
    ({"a": 1, "b": 3}, {"$expr": {"$gt": ["$a", "$b"]}}),
    (
        {"price": 100, "discount": 30},
        {"$expr": {"$lt": [{"$subtract": ["$price", "$discount"]}, 80]}},
    ),
    ({"a": 5, "b": 3, "name": "x"}, {"name": "x", "$expr": {"$gt": ["$a", "$b"]}}),
    ({}, {"$expr": "$missing"}),  # falsy
    ({"x": None}, {"$expr": "$x"}),  # falsy
    ({"a": 5}, {"$expr": {"$dateToString": {"date": "$a"}}}),  # unported op -> defer
    # $all — now handled in Rust (element equality via Python ==).
    ({"tags": ["a", "b", "c"]}, {"tags": {"$all": ["a", "b"]}}),
    ({"tags": ["a"]}, {"tags": {"$all": ["a", "b"]}}),
    ({"tags": [1, 2, 3]}, {"tags": {"$all": [1.0, 2.0]}}),  # numeric bridge
    ({"tags": []}, {"tags": {"$all": []}}),
    ({"tags": ["a", "b"]}, {"tags": {"$all": [Regex("^a")]}}),  # regex elem -> defer
    # --- regex ($regex/$options + bare BSON regex) — now handled in Rust ---
    ({"item": "paper"}, {"item": {"$regex": "^p"}}),
    ({"item": "journal"}, {"item": {"$regex": "^p"}}),
    ({"item": "Paper"}, {"item": {"$regex": "^p", "$options": "i"}}),
    ({"item": "Paper"}, {"item": {"$regex": "^p"}}),
    ({"name": "abc123"}, {"name": {"$regex": "[0-9]+"}}),
    ({"name": "abcdef"}, {"name": {"$regex": "[0-9]+"}}),
    ({"tags": ["red", "blank"]}, {"tags": {"$regex": "^bl"}}),  # array element
    ({"tags": ["red", "blank"]}, {"tags": {"$regex": "^z"}}),
    ({"x": "hello"}, {"x": Regex("^h")}),  # bare BSON regex literal
    ({"x": "Hello"}, {"x": Regex("^h", "i")}),
    ({"x": "Hello"}, {"x": Regex("^h")}),
    ({"x": "a\nb"}, {"x": {"$regex": "^b", "$options": "m"}}),  # multiline
    ({"x": "a\nb"}, {"x": {"$regex": "^b"}}),
    ({"x": "foobar"}, {"x": {"$regex": "o.b", "$options": "s"}}),
    ({"x": 5}, {"x": {"$regex": "5"}}),  # non-string value -> no match
    ({}, {"x": {"$regex": "anything"}}),  # missing field -> no match
    ({"x": r"(a)\1"}, {"x": {"$regex": r"(a)\1"}}),  # backref pattern -> Rust defers
    # --- geo (slices geo-1 / geo-1b): point docs vs region/near queries ---
    # $geoWithin $box — point inside / outside.
    ({"loc": [5.0, 5.0]}, {"loc": {"$geoWithin": {"$box": [[0.0, 0.0], [10.0, 10.0]]}}}),
    ({"loc": [50.0, 5.0]}, {"loc": {"$geoWithin": {"$box": [[0.0, 0.0], [10.0, 10.0]]}}}),
    # $geoWithin $polygon.
    (
        {"loc": [5.0, 5.0]},
        {"loc": {"$geoWithin": {"$polygon": [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]}}},
    ),
    (
        {"loc": [20.0, 20.0]},
        {"loc": {"$geoWithin": {"$polygon": [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]}}},
    ),
    # $geoWithin $geometry (GeoJSON Polygon) — GeoJSON Point + legacy {lng,lat} docs.
    (
        {"loc": {"type": "Point", "coordinates": [5.0, 5.0]}},
        {
            "loc": {
                "$geoWithin": {
                    "$geometry": {
                        "type": "Polygon",
                        "coordinates": [
                            [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]
                        ],
                    }
                }
            }
        },
    ),
    (
        {"loc": {"lng": 99.0, "lat": 99.0}},
        {
            "loc": {
                "$geoWithin": {
                    "$geometry": {
                        "type": "Polygon",
                        "coordinates": [
                            [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]
                        ],
                    }
                }
            }
        },
    ),
    # $geoIntersects $geometry (point in / out of polygon).
    (
        {"loc": [5.0, 5.0]},
        {
            "loc": {
                "$geoIntersects": {
                    "$geometry": {
                        "type": "Polygon",
                        "coordinates": [
                            [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]
                        ],
                    }
                }
            }
        },
    ),
    # $geoWithin $centerSphere (great-circle cap, radians) — in / out.
    ({"loc": [0.0, 0.0]}, {"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.1]}}}),
    ({"loc": [10.0, 10.0]}, {"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.1]}}}),
    # $near (legacy planar, bounded) — within / beyond max.
    ({"loc": [1.0, 1.0]}, {"loc": {"$near": [0.0, 0.0, 5.0]}}),
    ({"loc": [5.0, 5.0]}, {"loc": {"$near": [0.0, 0.0, 5.0]}}),
    # $nearSphere (GeoJSON, metres).
    (
        {"loc": [0.0, 0.0]},
        {
            "loc": {
                "$nearSphere": {
                    "$geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
                    "$maxDistance": 1500000.0,
                }
            }
        },
    ),
    # $center -> Rust returns None (Fallback to Python); curated test skips it.
    ({"loc": [1.0, 1.0]}, {"loc": {"$geoWithin": {"$center": [[0.0, 0.0], 5.0]}}}),
]


@pytest.mark.parametrize("doc,query", CURATED)
def test_curated_parity(doc, query):
    doc = bson.decode(bson.encode(doc))
    query = bson.decode(bson.encode(query))
    rust = _rust_match(doc, query)
    py = _pure.matches(doc, query)
    if rust is not None:
        assert rust == py, f"rust={rust} pure={py} for query={query} doc={doc}"


def _rand_scalar(rng):
    return rng.choice(
        [
            rng.randint(-50, 50),
            Int64(rng.randint(-50, 50)),
            round(rng.uniform(-50, 50), 2),
            Decimal128(str(rng.randint(-50, 50))),
            rng.choice(["a", "bb", "ccc", "apple", "banana"]),
            rng.choice([True, False]),
            None,
            ObjectId(),
        ]
    )


def _rand_doc(rng):
    d = {}
    for f in ("a", "b", "c"):
        r = rng.random()
        if r < 0.15:
            continue  # missing field
        elif r < 0.35:
            d[f] = [_rand_scalar(rng) for _ in range(rng.randint(0, 3))]
        elif r < 0.45:
            d[f] = {"n": _rand_scalar(rng)}
        else:
            d[f] = _rand_scalar(rng)
    return d


def _rand_query(rng):
    field = rng.choice(["a", "b", "c", "a.n", "a.0"])
    op = rng.choice(
        [
            "eq",
            "$gt",
            "$gte",
            "$lt",
            "$lte",
            "$in",
            "$nin",
            "$ne",
            "$exists",
            "$type",
            "$size",
            "$mod",
            "$all",
        ]
    )
    if op == "eq":
        return {field: _rand_scalar(rng)}
    if op in ("$in", "$nin", "$all"):
        return {field: {op: [_rand_scalar(rng) for _ in range(rng.randint(0, 3))]}}
    if op == "$exists":
        return {field: {op: rng.choice([True, False])}}
    if op == "$type":
        types = ["string", "int", "double", "number", "bool", "null", "array"]
        return {field: {op: rng.choice(types)}}
    if op == "$size":
        return {field: {op: rng.randint(0, 3)}}
    if op == "$mod":
        return {field: {op: [rng.randint(1, 5), rng.randint(0, 4)]}}
    return {field: {op: _rand_scalar(rng)}}


def test_randomised_fuzz_parity():
    rng = random.Random(0x5EC0)
    handled = 0
    for _ in range(6000):
        doc = bson.decode(bson.encode(_rand_doc(rng)))
        query = bson.decode(bson.encode(_rand_query(rng)))
        rust = _rust_match(doc, query)
        if rust is None:
            continue  # fallback case — shim would use pure Python
        handled += 1
        py = _pure.matches(doc, query)
        assert rust == py, f"divergence: rust={rust} pure={py} query={query} doc={doc}"
    assert handled > 1000, f"expected the Rust matcher to handle many cases, only {handled}"


def _rust_match_batch(docs, query, collation=None):
    res = _rust.query_matches_batch(
        bson.encode({"d": list(docs)}),
        bson.encode(query),
        bson.encode({}),
        bson.encode(collation or {}),
    )
    return None if res is None else bson.decode(res)["m"]


def test_batch_matches_parity():
    """The batched seam returns the same per-doc flags as per-doc matching, and
    defers the whole batch (None) iff any single doc would defer."""
    # empty query -> all True; empty list -> [].
    assert _rust_match_batch([{"a": 1}, {"a": 2}], {}) == [True, True]
    assert _rust_match_batch([], {"a": 1}) == []

    rng = random.Random(0xBA7C_4)
    handled = 0
    for _ in range(3000):
        docs = [bson.decode(bson.encode(_rand_doc(rng))) for _ in range(rng.randint(0, 6))]
        query = bson.decode(bson.encode(_rand_query(rng)))
        rust = _rust_match_batch(docs, query)
        per_doc = [_rust_match(d, query) for d in docs]
        if rust is None:
            # whole-batch fallback must mean at least one doc deferred per-doc too.
            assert any(r is None for r in per_doc) or not docs
            continue
        handled += 1
        py = [_pure.matches(d, query) for d in docs]
        assert rust == py, f"batch divergence: rust={rust} pure={py} query={query} docs={docs}"
    assert handled > 500, f"expected many handled batches, only {handled}"


# Patterns valid in both Python `re` and the Rust `regex` crate (no
# backreferences / lookaround / `\Z`, and no trailing-`\n`-sensitive anchoring,
# since the crate's `$` matches end-of-haystack only). Subjects are
# newline-free for the same reason.
_REGEX_PATTERNS = [
    "^a",
    "b$",
    "a.c",
    "[0-9]+",
    "[a-c]{2}",
    "ab|cd",
    "^.*z",
    "(ab)+",
    "x?y",
    "\\d",
    "\\w+",
    "[^abc]",
    "a{1,3}",
]
_REGEX_OPTIONS = ["", "i", "s", "x", "is"]
_REGEX_SUBJECTS = ["abc", "ABC", "a1b2", "xyz", "aabbcc", "cd", "zzz", "b", "", "Hello9"]


def test_regex_fuzz_parity():
    """Random $regex queries against random subjects: when the Rust matcher
    returns a concrete bool (didn't defer), it must equal pure-Python `re`."""
    rng = random.Random(0x5EC0_4E9E)
    handled = 0
    for _ in range(4000):
        pat = rng.choice(_REGEX_PATTERNS)
        opts = rng.choice(_REGEX_OPTIONS)
        subj = rng.choice(_REGEX_SUBJECTS)
        field = "x"
        if rng.random() < 0.5:
            query = {field: {"$regex": pat, "$options": opts}}
        else:
            query = {field: Regex(pat, opts)}
        doc = {field: subj}
        doc = bson.decode(bson.encode(doc))
        query = bson.decode(bson.encode(query))
        rust = _rust_match(doc, query)
        if rust is None:
            continue
        handled += 1
        py = _pure.matches(doc, query)
        assert rust == py, f"regex divergence: rust={rust} pure={py} query={query} doc={doc}"
    assert handled > 1000, f"expected many handled regex cases, only {handled}"
