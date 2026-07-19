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
from bson.code import Code

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
    dw, qw, vw, cw = (
        bson.encode(doc),
        bson.encode(query),
        bson.encode({}),
        bson.encode(collation or {}),
    )
    owned = _rust.query_matches(dw, qw, vw, cw)
    # The raw-BSON matcher must agree with the owned matcher bool-for-bool AND
    # defer (None) on exactly the same inputs — the two-sided contract. Every
    # curated / fuzz / regex / collation case below thus also pins matches_raw.
    raw = _rust.query_matches_raw(dw, qw, vw, cw)
    assert raw == owned, f"matches_raw={raw} != matches={owned} query={query} doc={doc}"
    return owned


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
    # JS-Code equality — Rust now matches by value, like Python.
    ({"f": Code("function () {}")}, {"f": Code("function () {}")}),
    ({"f": Code("function () {}")}, {"f": Code("other")}),
    ({"f": Code("c", {"a": 55})}, {"f": Code("c", {"a": 55})}),
    # JS-Code under range operators — pymongo's Code is a str subclass, so the
    # Python engine compares it as a plain string (scope ignored); the Rust
    # matcher now mirrors that instead of deferring.
    ({"f": Code("b")}, {"f": {"$gt": Code("a")}}),
    ({"f": Code("a")}, {"f": {"$gt": Code("b")}}),
    ({"f": Code("b")}, {"f": {"$gte": Code("b")}}),
    ({"f": Code("b")}, {"f": {"$lt": "c"}}),  # Code vs plain string
    ({"f": "b"}, {"f": {"$gt": Code("a")}}),  # string field vs Code bound
    ({"f": Code("b", {"s": 1})}, {"f": {"$gt": Code("a")}}),  # scope ignored
    ({"f": Code("b")}, {"f": {"$gt": 5}}),  # cross-bracket -> no match
    ({"f": 5}, {"f": {"$lt": Code("a")}}),  # cross-bracket -> no match
    # $all with regex elements matches array elements as patterns.
    ({"k": ["serialization", "test", "x"]}, {"k": {"$all": [Regex("ser"), Regex("test")]}}),
    ({"k": ["abc", "def"]}, {"k": {"$all": [Regex("zzz")]}}),
    # $all against a SCALAR field (mongod treats it like a one-element array):
    # equality and regex elements both match; $all: [] matches nothing.
    ({"k": "red"}, {"k": {"$all": ["red"]}}),
    ({"k": "red"}, {"k": {"$all": [Regex("^red$")]}}),
    ({"k": "red"}, {"k": {"$all": ["red", "blue"]}}),
    ({"k": ["a", "b"]}, {"k": {"$all": []}}),
    ({"k": "red"}, {"k": {"$all": []}}),
    ({"k": "red"}, {"k": {"$all": [{"$elemMatch": {"$eq": "red"}}]}}),
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
    # Range against an embedded-document bound: order field-by-field (key
    # compare, else recurse, else shorter-first); a document field vs a scalar
    # bound and a scalar field vs a document bound both no-match (type bracket).
    ({"a": {"x": 2}}, {"a": {"$gt": {"x": 1}}}),
    ({"a": {"x": 1}}, {"a": {"$gt": {"x": 1}}}),
    ({"a": {"x": 1}}, {"a": {"$gte": {"x": 1}}}),
    ({"a": {"x": 1, "y": 9}}, {"a": {"$gt": {"x": 1}}}),
    ({"a": {"y": 1}}, {"a": {"$gt": {"x": 1}}}),
    ({"a": {"x": 0}}, {"a": {"$lt": {"x": 1}}}),
    ({"a": {"x": 1}}, {"a": {"$lt": {"x": 1, "y": 5}}}),
    ({"a": {"x": 1}}, {"a": {"$gt": 2}}),
    ({"a": 2}, {"a": {"$gt": {"x": 1}}}),
    # Array-vs-array range: elements order by full BSON order (type rank), so a
    # cross-type element pair still orders (string element > number element).
    ({"a": [1, "x"]}, {"a": {"$gt": [1, 2]}}),
    ({"a": [1, "x"]}, {"a": {"$lt": [1, 2]}}),
    ({"a": ["x", 1]}, {"a": {"$gt": [1, 2]}}),
    ({"a": [2, "x"]}, {"a": {"$gt": [1, 2]}}),
    ({"a": [1]}, {"a": {"$lt": [1, 2]}}),
    ({"a": [1, 2]}, {"a": {"$gte": [1, 2]}}),
    # Range operators against an array-valued (multikey) field: match when any
    # element satisfies the bound; the array-as-a-whole is never compared to the
    # scalar bound (an array out-ranks a number in BSON type order).
    ({"dim": [14, 21]}, {"dim": {"$gt": 25}}),
    ({"dim": [22.85, 30]}, {"dim": {"$gt": 25}}),
    ({"dim": [10, 15.25]}, {"dim": {"$gt": 15, "$lt": 20}}),
    ({"dim": [14, 21]}, {"dim": {"$lte": 14}}),
    ({"dim": ["a", "z"]}, {"dim": {"$gt": "m"}}),
    # Array-vs-array bound: whole-array lexicographic comparison, identical to
    # Python's native `list < list` and to mongod. Pinned to the Python oracle.
    ({"a": [1, 3]}, {"a": {"$gt": [1, 2]}}),
    ({"a": [1, 2]}, {"a": {"$gt": [1, 2]}}),
    ({"a": [1, 2, 3]}, {"a": {"$gt": [1, 2]}}),
    ({"a": 5}, {"a": {"$gt": [1, 2]}}),
    ({"a": [2]}, {"a": {"$gt": [1, 2]}}),
    ({"a": [1, 3]}, {"a": {"$lt": [1, 3]}}),
    ({"a": [1, 2]}, {"a": {"$lt": [1, 3]}}),
    ({"a": [1, 2, 3]}, {"a": {"$lt": [1, 3]}}),
    ({"a": [2]}, {"a": {"$lt": [1, 3]}}),
    ({"a": [1, 3]}, {"a": {"$gte": [1, 2]}}),
    ({"a": [1, 2]}, {"a": {"$gte": [1, 2]}}),
    ({"a": [1, 2, 3]}, {"a": {"$gte": [1, 2]}}),
    ({"a": [2]}, {"a": {"$gte": [1, 2]}}),
    ({"a": [1, 2]}, {"a": {"$lte": [1, 2]}}),
    # Prefix ordering: [1,2] < [1,2,3] (shorter sorts first).
    ({"a": [1, 2]}, {"a": {"$lt": [1, 2, 3]}}),
    ({"a": [1, 2, 3]}, {"a": {"$gt": [1, 2]}}),
    # Cross-type element pair after equal leading elements -> no match (Python's
    # `list < list` raises TypeError -> swallowed; Rust returns a clean False).
    ({"a": [1, "x"]}, {"a": {"$gt": [1, 2]}}),
    ({"a": [1, "x"]}, {"a": {"$lt": [1, 2]}}),
    ({"a": [2, "x"]}, {"a": {"$gt": [1, 2]}}),  # decisive first pair 2>1
    # Array field vs scalar bound still rides the multikey element path.
    ({"a": [1, 3]}, {"a": {"$gt": 2}}),
    ({"a": [1, 2]}, {"a": {"$gt": 2}}),
    # Cross-type numeric elements (int vs double) compare by value, like Python.
    ({"a": [1, 2.5]}, {"a": {"$gt": [1, 2]}}),
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
    ({"a": 5}, {"a": {"$type": 16}}),  # numeric code
    ({"a": 5}, {"a": {"$type": 2.0}}),  # whole-double code now computes on both
    ({"a": 5}, {"a": {"$type": 16.0}}),  # whole-double int code -> matches
    ({"a": 5}, {"a": {"$type": -1}}),  # minKey code (valid, no match)
    ({"tags": [1, 2, 3]}, {"tags": {"$size": 3}}),
    ({"tags": [1, 2]}, {"tags": {"$size": 3}}),
    ({"tags": "abc"}, {"tags": {"$size": 3}}),
    # An integer-valued float $size is accepted (== 2). (Invalid $size args —
    # negative / non-integer / string / bool — RAISE on both engines, so they're
    # covered by the unit test, not the parity corpus which compares bool results.)
    ({"tags": [1, 2]}, {"tags": {"$size": 2.0}}),
    ({"tags": [1]}, {"tags": {"$size": 2.0}}),
    ({"n": 12}, {"n": {"$mod": [4, 0]}}),
    ({"n": 13}, {"n": {"$mod": [4, 1]}}),
    ({"n": 13}, {"n": {"$mod": [4, 0]}}),
    ({"vals": [3, 7, 12]}, {"vals": {"$mod": [4, 0]}}),
    # $mod: double values truncate toward zero, divisor truncates too, bool is
    # excluded, C-style modulo (-5 % 2 == -1). (Decimal128 defers on the Rust
    # side — parity harness skips a defer.)
    ({"n": 5.0}, {"n": {"$mod": [2, 1]}}),
    ({"n": 5.5}, {"n": {"$mod": [2, 1]}}),
    ({"n": 4.9}, {"n": {"$mod": [2, 0]}}),
    ({"n": 4.9}, {"n": {"$mod": [2.5, 0]}}),
    ({"n": True}, {"n": {"$mod": [2, 1]}}),
    ({"n": -5}, {"n": {"$mod": [2, 1]}}),
    ({"n": -5}, {"n": {"$mod": [2, -1]}}),
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
    # Cross-type range against a document-valued field / a document bound:
    # mongod's range operators are type-bracketed, so these never match. Python's
    # native `<` on dicts raises TypeError (no match); the Rust matcher mirrors it
    # with a clean no-match instead of a Fallback (was: deferred / Rust-server error).
    ({"a": {"x": 1}}, {"a": {"$gt": 2}}),
    ({"a": {"x": 1}}, {"a": {"$lt": 2}}),
    ({"a": {"x": 1}}, {"a": {"$gte": 2}}),
    ({"a": 2}, {"a": {"$gt": {"x": 1}}}),
    ({"a": {"x": 2}}, {"a": {"$gt": {"x": 1}}}),  # doc-vs-doc still no-match (dicts unorderable)
    # $elemMatch: {$gt: n} over an array of sub-documents — the differential case.
    ({"items": [{"k": 1}, {"k": 2}]}, {"items": {"$elemMatch": {"$gt": 2}}}),
    ({"items": [{"k": 1}, {"k": 2}]}, {"items": {"$elemMatch": {"$lt": 2}}}),
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
    # whole-number-double mask / bit positions compute on both engines (mongod
    # accepts them, truncating) — the case the Rust core used to wrongly defer.
    ({"flags": 0b0110}, {"flags": {"$bitsAllSet": 6.0}}),
    ({"flags": 0b0110}, {"flags": {"$bitsAllSet": [1.0, 2.0]}}),
    ({"flags": 0b0110}, {"flags": {"$bitsAnyClear": 8.0}}),
    # $gte/$lte: null match null + missing (like $eq: null); $gt: null nothing.
    ({"f": None}, {"f": {"$gte": None}}),
    ({}, {"f": {"$gte": None}}),
    ({"f": 5}, {"f": {"$gte": None}}),
    ({"f": None}, {"f": {"$lte": None}}),
    ({"f": 5}, {"f": {"$gt": None}}),
    # $exists mongod truthiness: empty string / array / doc are truthy.
    ({"f": 5}, {"f": {"$exists": ""}}),
    ({"f": 5}, {"f": {"$exists": []}}),
    ({"f": 5}, {"f": {"$exists": {}}}),
    ({}, {"f": {"$exists": 0}}),
    ({"f": 5}, {"f": {"$exists": 1}}),
    ({"x": Decimal128("5")}, {"x": 5}),
    ({"x": 5}, {"x": Decimal128("5")}),
    ({"x": Decimal128("3.5")}, {"x": 3.5}),
    ({"x": Decimal128("5")}, {"x": 6}),
    ({"x": [Decimal128("5"), Decimal128("6")]}, {"x": 6}),
    ({"x": Decimal128("5")}, {"x": {"$in": [3, 5, 7]}}),
    ({"x": True}, {"x": 1}),
    ({"x": 1}, {"x": True}),
    ({"x": False}, {"x": 0}),
    # bool is its own range bracket ($gt/$lt/$gte/$lte): a bool compares only with
    # another bool (True > False), never with a number or any other type — mongod
    # brackets bool away from numbers, so all the bool-vs-number cases no-match.
    ({"x": True}, {"x": {"$gt": 0}}),  # bool vs number -> no match
    ({"x": True}, {"x": {"$lt": 1}}),  # bool vs number -> no match
    ({"x": True}, {"x": {"$gte": 1}}),  # bool vs number -> no match
    ({"x": False}, {"x": {"$lt": 1}}),  # bool vs number -> no match
    ({"x": 5}, {"x": {"$gt": True}}),  # number vs bool bound -> no match
    ({"x": 0}, {"x": {"$lt": True}}),  # number vs bool bound -> no match
    ({"x": True}, {"x": {"$gt": False}}),  # bool vs bool -> True > False -> match
    ({"x": False}, {"x": {"$gte": False}}),  # bool vs bool -> match
    ({"x": False}, {"x": {"$gt": False}}),  # bool vs bool -> no match
    ({"x": True}, {"x": {"$gt": Int64(0)}}),  # bool vs long -> no match
    ({"x": True}, {"x": {"$gt": "a"}}),  # bool vs string -> no match
    ({"x": True}, {"x": {"$gt": Decimal128("0.5")}}),  # bool vs decimal -> no match
    ({"x": [True, False]}, {"x": {"$gt": 0}}),  # multikey bool elements vs number -> no match
    ({"x": [True, False]}, {"x": {"$gt": False}}),  # multikey bool elements vs bool -> match
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
    # $dateToString inside $expr on a real date (a non-date now correctly raises
    # Location16006 on both engines — see test_date_misc_typeguard_defers_and_raises).
    ({"a": datetime.datetime(2026, 1, 1)}, {"$expr": {"$dateToString": {"date": "$a"}}}),
    # $all — now handled in Rust (element equality via Python ==).
    ({"tags": ["a", "b", "c"]}, {"tags": {"$all": ["a", "b"]}}),
    ({"tags": ["a"]}, {"tags": {"$all": ["a", "b"]}}),
    ({"tags": [1, 2, 3]}, {"tags": {"$all": [1.0, 2.0]}}),  # numeric bridge
    ({"tags": []}, {"tags": {"$all": []}}),
    ({"tags": ["a", "b"]}, {"tags": {"$all": [Regex("^a")]}}),  # regex elem (pattern match)
    # $all with $elemMatch clauses — each clause needs some element to match.
    ({"a": [1, 2, 3]}, {"a": {"$all": [{"$elemMatch": {"$gt": 1, "$lt": 3}}]}}),
    ({"a": [4, 5]}, {"a": {"$all": [{"$elemMatch": {"$gt": 1, "$lt": 3}}]}}),  # no match
    (
        {"a": [1, 5, 10]},
        {"a": {"$all": [{"$elemMatch": {"$gt": 4}}, {"$elemMatch": {"$lt": 2}}]}},
    ),
    # (A mixed $all — scalar + $elemMatch — is now rejected by both engines;
    # covered by test_all_invalid_defers_and_raises, not here.)
    # $in / $nin with a regex candidate — matches string values by pattern.
    ({"s": "hello"}, {"s": {"$in": [Regex("^h", "i")]}}),
    ({"s": "World"}, {"s": {"$in": [Regex("^h", "i")]}}),  # no match
    ({"s": "HELLO"}, {"s": {"$in": [Regex("^h", "i")]}}),  # case-insensitive
    ({"s": "abc"}, {"s": {"$in": ["x", Regex("^a"), "y"]}}),  # mixed literal + regex
    ({"s": "world"}, {"s": {"$nin": [Regex("^h")]}}),  # nin keeps non-matching
    ({"s": "hello"}, {"s": {"$nin": [Regex("^h")]}}),  # nin excludes matching
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
    # backref / lookaround now compile via fancy-regex (linear engine can't),
    # so Rust evaluates them instead of deferring — must match Python `re`.
    ({"x": "aa"}, {"x": {"$regex": r"(a)\1"}}),  # backreference -> match
    ({"x": r"(a)\1"}, {"x": {"$regex": r"(a)\1"}}),  # one 'a' -> no match
    ({"x": "foobar"}, {"x": {"$regex": r"foo(?!baz)"}}),  # neg lookahead -> match
    ({"x": "foobaz"}, {"x": {"$regex": r"foo(?!baz)"}}),  # neg lookahead -> no match
    ({"x": "systemcoll"}, {"x": {"$regex": r"^(?!system\.)"}}),  # listColl filter -> match
    ({"x": "system.foo"}, {"x": {"$regex": r"^(?!system\.)"}}),  # -> no match
    ({"x": "Foobar"}, {"x": {"$regex": r"foo(?!baz)", "$options": "i"}}),  # flags + lookahead
    ({"x": "xyzabc"}, {"x": {"$regex": r"(?<=xyz)abc"}}),  # lookbehind -> match
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
    # $geoWithin $center (planar disk) — in / out. Rust uses an exact disk and
    # Python a Shapely 64-gon buffer; they agree away from the sub-degree boundary
    # annulus, so these points sit well inside / outside.
    ({"loc": [1.0, 1.0]}, {"loc": {"$geoWithin": {"$center": [[0.0, 0.0], 5.0]}}}),
    ({"loc": [40.0, 40.0]}, {"loc": {"$geoWithin": {"$center": [[0.0, 0.0], 5.0]}}}),
    # $near legacy 2d sibling form ({$near: [x,y], $maxDistance, $minDistance}) —
    # the shape Java's Filters.near builds. In / out of the planar bound.
    ({"loc": [1.0, 1.0]}, {"loc": {"$near": [0.0, 0.0], "$maxDistance": 5.0}}),
    ({"loc": [5.0, 5.0]}, {"loc": {"$near": [0.0, 0.0], "$maxDistance": 5.0}}),
    ({"loc": [1.0, 1.0]}, {"loc": {"$near": [0.0, 0.0], "$maxDistance": 5.0, "$minDistance": 3.0}}),
    # $nearSphere legacy sibling form ($maxDistance in radians on the unit sphere).
    ({"loc": [0.0, 0.0]}, {"loc": {"$nearSphere": [0.0, 0.0], "$maxDistance": 0.1}}),
    ({"loc": [10.0, 10.0]}, {"loc": {"$nearSphere": [0.0, 0.0], "$maxDistance": 0.1}}),
    # $jsonSchema — the bounded keyword subset the pure server validates.
    # bsonType (alias + numeric code + list), type (JSON type).
    ({"name": "Joe"}, {"$jsonSchema": {"properties": {"name": {"bsonType": "string"}}}}),
    ({"name": 5}, {"$jsonSchema": {"properties": {"name": {"bsonType": "string"}}}}),
    ({"n": 5}, {"$jsonSchema": {"properties": {"n": {"bsonType": ["int", "long"]}}}}),
    ({"n": 5}, {"$jsonSchema": {"properties": {"n": {"bsonType": 16}}}}),  # numeric code
    ({"n": 5.0}, {"$jsonSchema": {"properties": {"n": {"type": "number"}}}}),
    ({"n": 5}, {"$jsonSchema": {"properties": {"n": {"type": "integer"}}}}),
    ({"n": 5.5}, {"$jsonSchema": {"properties": {"n": {"type": "integer"}}}}),  # double !integer
    # Draft-4 exclusive bounds (booleans sharpening minimum/maximum, per mongod).
    ({"n": 6}, {"$jsonSchema": {"properties": {"n": {"minimum": 6, "exclusiveMinimum": True}}}}),
    ({"n": 7}, {"$jsonSchema": {"properties": {"n": {"minimum": 6, "exclusiveMinimum": True}}}}),
    ({"n": 6}, {"$jsonSchema": {"properties": {"n": {"minimum": 6, "exclusiveMinimum": False}}}}),
    ({"n": 6}, {"$jsonSchema": {"properties": {"n": {"maximum": 6, "exclusiveMaximum": True}}}}),
    ({"n": 5}, {"$jsonSchema": {"properties": {"n": {"maximum": 6, "exclusiveMaximum": True}}}}),
    # multipleOf (fmod semantics, incl. a fractional divisor).
    ({"n": 6}, {"$jsonSchema": {"properties": {"n": {"multipleOf": 3}}}}),
    ({"n": 7}, {"$jsonSchema": {"properties": {"n": {"multipleOf": 3}}}}),
    ({"n": 7.5}, {"$jsonSchema": {"properties": {"n": {"multipleOf": 2.5}}}}),
    ({"n": 6}, {"$jsonSchema": {"properties": {"n": {"multipleOf": 2.5}}}}),
    # Tuple-form items + additionalItems (false / schema / absent).
    ({"a": [1, "x"]}, {"$jsonSchema": {"properties": {"a": {"items": [{"bsonType": "int"}]}}}}),
    (
        {"a": [1, "x"]},
        {
            "$jsonSchema": {
                "properties": {"a": {"items": [{"bsonType": "int"}], "additionalItems": False}}
            }
        },
    ),
    (
        {"a": [1, "x"]},
        {
            "$jsonSchema": {
                "properties": {
                    "a": {"items": [{"bsonType": "int"}], "additionalItems": {"bsonType": "string"}}
                }
            }
        },
    ),
    (
        {"a": [1, True]},
        {
            "$jsonSchema": {
                "properties": {
                    "a": {"items": [{"bsonType": "int"}], "additionalItems": {"bsonType": "string"}}
                }
            }
        },
    ),
    # title / description are accepted-and-ignored metadata.
    (
        {"n": 5},
        {"$jsonSchema": {"title": "t", "description": "d", "properties": {"n": {"minimum": 1}}}},
    ),
    # required + top-level.
    ({"a": 1, "b": 2}, {"$jsonSchema": {"required": ["a", "b"]}}),
    ({"a": 1}, {"$jsonSchema": {"required": ["a", "b"]}}),  # missing b -> False
    ({"a": 1}, {"$jsonSchema": {"bsonType": "object", "required": ["a"]}}),
    # numeric bounds (min/max/exclusive) — only apply to numeric values.
    ({"age": 30}, {"$jsonSchema": {"properties": {"age": {"minimum": 0, "maximum": 120}}}}),
    ({"age": -1}, {"$jsonSchema": {"properties": {"age": {"minimum": 0}}}}),  # below min
    # (Draft-6 numeric exclusive bounds are rejected at parse time now — the
    # draft-4 boolean form mongod implements is covered above.)
    (
        {"age": 5},
        {"$jsonSchema": {"properties": {"age": {"minimum": 5, "exclusiveMinimum": True}}}},
    ),
    (
        {"age": 5},
        {"$jsonSchema": {"properties": {"age": {"maximum": 10, "exclusiveMaximum": True}}}},
    ),
    # string length + pattern.
    ({"s": "abc"}, {"$jsonSchema": {"properties": {"s": {"minLength": 2, "maxLength": 4}}}}),
    ({"s": "a"}, {"$jsonSchema": {"properties": {"s": {"minLength": 2}}}}),  # too short
    ({"s": "hello"}, {"$jsonSchema": {"properties": {"s": {"pattern": "^h"}}}}),
    ({"s": "world"}, {"$jsonSchema": {"properties": {"s": {"pattern": "^h"}}}}),  # no match
    # array items + counts.
    ({"xs": [1, 2, 3]}, {"$jsonSchema": {"properties": {"xs": {"minItems": 2, "maxItems": 5}}}}),
    ({"xs": [1]}, {"$jsonSchema": {"properties": {"xs": {"minItems": 2}}}}),  # too few
    ({"xs": [1, 2]}, {"$jsonSchema": {"properties": {"xs": {"items": {"bsonType": "int"}}}}}),
    ({"xs": [1, "x"]}, {"$jsonSchema": {"properties": {"xs": {"items": {"bsonType": "int"}}}}}),
    # uniqueItems — distinct scalars pass; a duplicate (incl. cross-type-equal
    # numeric 1 == 1.0 at top level) or duplicate documents fail; false is a no-op.
    ({"xs": [1, 2, 3]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    ({"xs": [1, 2, 2]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    ({"xs": [1, 1.0]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    ({"xs": ["a", "b", "a"]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    ({"xs": [{"a": 1}, {"a": 2}]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    ({"xs": [{"a": 1}, {"a": 1}]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    # nested cross-type-equal numerics collide recursively ({a:1} == {a:1.0}).
    (
        {"xs": [{"a": 1}, {"a": 1.0}]},
        {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}},
    ),
    ({"xs": [[1, 2], [1.0, 2.0]]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    ({"xs": [[1, 2], [1, 3]]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": True}}}}),
    ({"xs": [1, 1]}, {"$jsonSchema": {"properties": {"xs": {"uniqueItems": False}}}}),
    # enum.
    ({"c": "red"}, {"$jsonSchema": {"properties": {"c": {"enum": ["red", "green"]}}}}),
    ({"c": "blue"}, {"$jsonSchema": {"properties": {"c": {"enum": ["red", "green"]}}}}),
    # object property counts.
    ({"o": {"a": 1, "b": 2}}, {"$jsonSchema": {"properties": {"o": {"maxProperties": 3}}}}),
    ({"o": {"a": 1, "b": 2}}, {"$jsonSchema": {"properties": {"o": {"minProperties": 3}}}}),
    # nested: properties within properties + required.
    (
        {"user": {"name": "Al", "age": 3}},
        {
            "$jsonSchema": {
                "properties": {
                    "user": {
                        "bsonType": "object",
                        "required": ["name"],
                        "properties": {"age": {"minimum": 0}},
                    }
                }
            }
        },
    ),
    # allOf — all sub-schemas must hold.
    (
        {"n": 5},
        {"$jsonSchema": {"properties": {"n": {"allOf": [{"bsonType": "int"}, {"minimum": 0}]}}}},
    ),
    (
        {"n": -1},
        {"$jsonSchema": {"properties": {"n": {"allOf": [{"bsonType": "int"}, {"minimum": 0}]}}}},
    ),
    # anyOf — at least one.
    (
        {"x": "s"},
        {
            "$jsonSchema": {
                "properties": {"x": {"anyOf": [{"bsonType": "string"}, {"bsonType": "int"}]}}
            }
        },
    ),
    (
        {"x": 1.5},
        {
            "$jsonSchema": {
                "properties": {"x": {"anyOf": [{"bsonType": "string"}, {"bsonType": "int"}]}}
            }
        },
    ),
    # oneOf — exactly one (both bounds match -> fail).
    (
        {"n": 5},
        {"$jsonSchema": {"properties": {"n": {"oneOf": [{"minimum": 0}, {"bsonType": "string"}]}}}},
    ),
    (
        {"n": 5},
        {"$jsonSchema": {"properties": {"n": {"oneOf": [{"minimum": 0}, {"maximum": 10}]}}}},
    ),
    # not — must NOT match.
    ({"x": "s"}, {"$jsonSchema": {"properties": {"x": {"not": {"bsonType": "int"}}}}}),
    ({"x": 5}, {"$jsonSchema": {"properties": {"x": {"not": {"bsonType": "int"}}}}}),
    # additionalProperties: false forbids extras; true / schema allow.
    ({"a": 1}, {"$jsonSchema": {"properties": {"a": {}}, "additionalProperties": False}}),
    ({"a": 1, "b": 2}, {"$jsonSchema": {"properties": {"a": {}}, "additionalProperties": False}}),
    ({"a": 1, "b": 2}, {"$jsonSchema": {"properties": {"a": {}}, "additionalProperties": True}}),
    (
        {"a": 1, "b": "x"},
        {"$jsonSchema": {"properties": {"a": {}}, "additionalProperties": {"bsonType": "string"}}},
    ),
    (
        {"a": 1, "b": 2},
        {"$jsonSchema": {"properties": {"a": {}}, "additionalProperties": {"bsonType": "string"}}},
    ),
    # top-level combinator over the whole document.
    ({"t": "x"}, {"$jsonSchema": {"anyOf": [{"required": ["t"]}, {"required": ["u"]}]}}),
    # patternProperties — keys matching the regex validate against the sub-schema.
    ({"s_a": "x", "n": 5}, {"$jsonSchema": {"patternProperties": {"^s_": {"bsonType": "string"}}}}),
    ({"s_a": 5}, {"$jsonSchema": {"patternProperties": {"^s_": {"bsonType": "string"}}}}),
    # additionalProperties: false with patternProperties allowing s_* keys.
    (
        {"id": 1, "s_x": 2},
        {
            "$jsonSchema": {
                "properties": {"id": {}},
                "patternProperties": {"^s_": {}},
                "additionalProperties": False,
            }
        },
    ),
    (
        {"id": 1, "other": 2},
        {
            "$jsonSchema": {
                "properties": {"id": {}},
                "patternProperties": {"^s_": {}},
                "additionalProperties": False,
            }
        },
    ),
    # dependencies — property (list) form and schema form.
    ({"card": 1, "billing": 2}, {"$jsonSchema": {"dependencies": {"card": ["billing"]}}}),
    ({"card": 1}, {"$jsonSchema": {"dependencies": {"card": ["billing"]}}}),
    ({"x": 1}, {"$jsonSchema": {"dependencies": {"card": ["billing"]}}}),  # trigger absent -> ok
    (
        {"a": 1, "b": 2},
        {
            "$jsonSchema": {
                "dependencies": {"a": {"required": ["b"], "properties": {"b": {"bsonType": "int"}}}}
            }
        },
    ),
    (
        {"a": 1, "b": "x"},
        {
            "$jsonSchema": {
                "dependencies": {"a": {"required": ["b"], "properties": {"b": {"bsonType": "int"}}}}
            }
        },
    ),
]


@pytest.mark.parametrize("doc,query", CURATED)
def test_curated_parity(doc, query):
    doc = bson.decode(bson.encode(doc))
    query = bson.decode(bson.encode(query))
    rust = _rust_match(doc, query)
    py = _pure.matches(doc, query)
    if rust is not None:
        assert rust == py, f"rust={rust} pure={py} for query={query} doc={doc}"


@pytest.mark.parametrize(
    "query",
    [
        {"a": {"$in": 5}},  # non-array
        {"a": {"$nin": "x"}},  # non-array
        {"a": {"$in": [{"$regex": "x"}]}},  # nested $ doc element
        {"a": {"$in": [{"$x": 1}]}},  # nested $ key
    ],
)
def test_in_nin_invalid_defers_and_raises(query):
    # An invalid $in/$nin: the Rust core defers (None) and the pure engine raises
    # BadValue — the two agree on "reject", so parity holds via the defer contract.
    doc = bson.decode(bson.encode({"a": 5}))
    query = bson.decode(bson.encode({"q": query}))["q"]
    assert _rust_match(doc, query) is None
    with pytest.raises(_pure.QueryError) as exc:
        _pure.matches(doc, query)
    assert exc.value.code == 2


@pytest.mark.parametrize(
    "query",
    [
        {"a": {"$not": 5}},  # scalar
        {"a": {"$not": "x"}},  # string
        {"a": {"$not": []}},  # array
        {"a": {"$not": {}}},  # empty doc
        {"a": {"$not": True}},  # bool
        {"a": {"$elemMatch": 5}},  # non-object
        {"a": {"$elemMatch": "x"}},  # non-object
    ],
)
def test_not_elemmatch_invalid_defers_and_raises(query):
    # Invalid $not/$elemMatch: Rust defers (None), pure engine raises BadValue.
    doc = bson.decode(bson.encode({"a": [1, 2, 3]}))
    query = bson.decode(bson.encode({"q": query}))["q"]
    assert _rust_match(doc, query) is None
    with pytest.raises(_pure.QueryError) as exc:
        _pure.matches(doc, query)
    assert exc.value.code == 2


@pytest.mark.parametrize(
    "query",
    [
        {"a": {"$all": 5}},  # non-array
        {"a": {"$all": [1, {"$elemMatch": {"x": 1}}]}},  # mixed elemMatch + scalar
        {"a": {"$all": [{"$gt": 1}]}},  # non-elemMatch $-doc
    ],
)
def test_all_invalid_defers_and_raises(query):
    # Invalid $all: Rust defers (None), pure engine raises BadValue.
    doc = bson.decode(bson.encode({"a": [1, 2, 3]}))
    query = bson.decode(bson.encode({"q": query}))["q"]
    assert _rust_match(doc, query) is None
    with pytest.raises(_pure.QueryError) as exc:
        _pure.matches(doc, query)
    assert exc.value.code == 2


@pytest.mark.parametrize(
    "t,code",
    [
        ("notatype", 2),  # unknown alias
        (0, 2),  # invalid code
        (100, 2),  # out-of-range code
        (2.5, 2),  # fractional code
        (True, 14),  # bool
        (["int", "notatype"], 2),  # array with a bad element
    ],
)
def test_type_invalid_defers_and_raises(t, code):
    # Invalid $type: Rust defers (None), pure engine raises the mongod code.
    doc = bson.decode(bson.encode({"a": 5}))
    query = bson.decode(bson.encode({"q": {"a": {"$type": t}}}))["q"]
    assert _rust_match(doc, query) is None
    with pytest.raises(_pure.QueryError) as exc:
        _pure.matches(doc, query)
    assert exc.value.code == code


@pytest.mark.parametrize(
    "query,code",
    [
        ({"s": {"$regex": "h", "$options": "z"}}, 51108),  # bad flag
        ({"s": {"$regex": "h", "$options": 5}}, 2),  # non-string options
        ({"s": {"$options": "i"}}, 2),  # $options without $regex
        ({"s": {"$regex": 5}}, 2),  # non-string pattern
    ],
)
def test_regex_options_invalid_defers_and_raises(query, code):
    # Invalid $regex/$options: the Rust core defers (None) and the pure engine
    # raises the mongod code — agreeing on "reject" via the defer contract.
    doc = bson.decode(bson.encode({"s": "hello"}))
    query = bson.decode(bson.encode({"q": query}))["q"]
    assert _rust_match(doc, query) is None
    with pytest.raises(_pure.QueryError) as exc:
        _pure.matches(doc, query)
    assert exc.value.code == code


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
