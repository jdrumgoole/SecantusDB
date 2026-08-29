"""Parity: Rust `_secantus_core.apply_update` vs pure-Python `apply_update`.

Phase 1 net for the third ported leaf engine. For each (doc, update) the Rust
path is run over BSON bytes; when it returns concrete bytes (didn't defer), the
decoded result must equal the authoritative pure-Python `apply_update`. When it
returns None (fallback — pipeline updates, positional ops, `$currentDate`,
`$min`/`$max`/`$pull`/`$addToSet`/`$bit`, error cases)
there's nothing to assert: the shim runs pure Python anyway.

Import-light: prefers the real `secantus.update`, else loads `update.py` +
`paths.py` by path under a stub `secantus` package (the corpus avoids
pipeline/positional/arrayFilters/`$currentDate` so the pure path never needs
`secantus.query` / `secantus.aggregate`).
"""

from __future__ import annotations

import importlib.util
import pathlib
import random
import sys
import types

import bson
import pytest
from bson import Int64, ObjectId
from bson.decimal128 import Decimal128

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")


def _load_pure_update():
    try:
        from secantus import update as u

        return u
    except Exception:
        pass
    root = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
    if "secantus" not in sys.modules:
        pkg = types.ModuleType("secantus")
        pkg.__path__ = [str(root)]
        sys.modules["secantus"] = pkg
    for name in ("paths", "update"):
        full = f"secantus.{name}"
        if full not in sys.modules:
            spec = importlib.util.spec_from_file_location(full, root / f"{name}.py")
            mod = importlib.util.module_from_spec(spec)
            sys.modules[full] = mod
            spec.loader.exec_module(mod)
    return sys.modules["secantus.update"]


_pure = _load_pure_update()


def _rust_apply(doc, update, is_upsert=False):
    return _rust.apply_update(bson.encode(doc), bson.encode(update), is_upsert)


# (doc, update, is_upsert). Mirrors tests/test_update.py for the handled ops,
# plus dotted/array paths, width-sensitive arithmetic, and _id immutability.
CURATED = [
    ({"a": 1}, {"$set": {"b": 2}}, False),
    ({}, {"$set": {"a.b.c": 5}}, False),
    ({"a": 1, "b": 2}, {"$unset": {"b": ""}}, False),
    ({"a": 1}, {"$inc": {"a": 2}}, False),
    ({}, {"$inc": {"a": 5}}, False),
    ({"a": 1}, {"$inc": {"a": 0.5}}, False),
    ({"a": Int64(5)}, {"$inc": {"a": 3}}, False),
    ({"a": 2}, {"$mul": {"a": 3}}, False),
    ({"a": 2}, {"$mul": {"a": 1.5}}, False),
    ({}, {"$mul": {"a": 4}}, False),
    # $inc / $mul on an *absent* field -> treat as 0 and apply (Rust computes).
    ({"other": 1}, {"$inc": {"n": 5}}, False),
    ({"other": 1}, {"$mul": {"n": 5}}, False),
    # $inc / $mul on an *explicit-null* field -> Rust defers so the pure-Python
    # engine raises TypeMismatch (code 14). rust returns None => no-assert skip.
    ({"n": None}, {"$inc": {"n": 5}}, False),
    ({"n": None}, {"$mul": {"n": 5}}, False),
    ({"b": 1}, {"$bit": {"b": {"and": 0}}}, False),
    ({"b": 5}, {"$bit": {"b": {"or": 2}}}, False),
    ({"b": 6}, {"$bit": {"b": {"xor": 3}}}, False),
    ({"b": Int64(12)}, {"$bit": {"b": {"and": 10}}}, False),
    ({}, {"$bit": {"b": {"or": 7}}}, False),
    # Multiple bit ops applied in order.
    ({"b": 0b1100}, {"$bit": {"b": {"and": 0b1010, "or": 0b0001}}}, False),
    ({"b": 0b1000}, {"$bit": {"b": {"or": 0b0001, "xor": 0b1001}}}, False),
    ({}, {"$push": {"tags": "x"}}, False),
    ({"tags": ["x"]}, {"$push": {"tags": "y"}}, False),
    # $push / $addToSet $each modifiers: multi-append, $position, $slice, $sort.
    ({"a": [3, 1, 2]}, {"$push": {"a": {"$each": [5, 4]}}}, False),
    ({}, {"$push": {"a": {"$each": [1, 2, 3]}}}, False),
    ({"a": [1, 2, 3]}, {"$push": {"a": {"$each": [9], "$slice": -2}}}, False),
    ({"a": [1, 2, 3]}, {"$push": {"a": {"$each": [9], "$slice": 2}}}, False),
    ({"a": [1, 2, 3]}, {"$push": {"a": {"$each": [9, 8], "$position": 1}}}, False),
    ({"a": [1, 2, 3]}, {"$push": {"a": {"$each": [9], "$position": -1}}}, False),
    ({"a": [1, 2]}, {"$addToSet": {"a": {"$each": [2, 3, 3, 4]}}}, False),
    ({}, {"$addToSet": {"a": {"$each": [1, 1, 2]}}}, False),
    ({"a": [1, 2, 3]}, {"$pop": {"a": 1}}, False),
    ({"a": [1, 2, 3]}, {"$pop": {"a": -1}}, False),
    ({"a": []}, {"$pop": {"a": 1}}, False),
    ({"a": 1}, {"$rename": {"a": "b"}}, False),
    ({"a": {"b": 1}}, {"$rename": {"a.b": "a.c"}}, False),
    # $rename validation: valid renames compute on both engines; an array-element
    # source/dest, same-field, same-path, empty, or non-string target raise on
    # Python and defer on Rust (which the server renders as BadValue).
    ({"a": 5, "arr": [1, 2, 3]}, {"$rename": {"a": "z"}}, False),
    ({"a": 5, "arr": [1, 2, 3]}, {"$rename": {"arr.0": "x"}}, False),
    ({"a": 5, "arr": [1, 2, 3]}, {"$rename": {"a": "arr.0"}}, False),
    ({"a": 5}, {"$rename": {"a": "a"}}, False),
    ({"a": 5}, {"$rename": {"a": "a.b"}}, False),
    ({"a": 5}, {"$rename": {"a": ""}}, False),
    ({"a": 5}, {"$rename": {"a": 5}}, False),
    ({"_id": 1, "a": 1, "b": 2}, {"x": 99}, False),
    ({"_id": 1, "n": 5}, {"$setOnInsert": {"created": True}}, False),
    ({}, {"$setOnInsert": {"created": True}}, True),
    ({"a": 1}, {"$set": {"a": 1}}, False),  # _id-free, value unchanged
    ({"vals": [10, 20, 30]}, {"$set": {"vals.1": 99}}, False),
    ({"a": {"b": {"c": 5}}}, {"$unset": {"a.b.c": ""}}, False),
    ({"n": 5}, {"$set": {"n": 5}, "$inc": {"m": 1}}, False),
    ({}, {}, False),  # empty update
    ({"a": 1}, {"b": 2, "c": [1, 2, {"d": 3}]}, False),  # replacement, no _id
    # $min / $max — BSON cross-type order; missing set, explicit-null compared.
    ({"a": 5}, {"$min": {"a": 3}}, False),  # 3 < 5 -> 3
    ({"a": 5}, {"$min": {"a": 7}}, False),  # no change
    ({}, {"$min": {"a": 4}}, False),  # absent -> set
    ({"a": None}, {"$max": {"a": 9}}, False),  # 9 > null -> set
    ({"a": None}, {"$min": {"a": 9}}, False),  # null < 9 -> keep null
    ({"a": 5}, {"$max": {"a": 9}}, False),
    # $min / $max over the previously-deferred types, now computed via the
    # `order::bson_lt` port: bool (own rank above numbers), Decimal128 (unified
    # numeric), NaN (unordered -> `<` False both ways), Binary bytes,
    # Timestamp, and regex (Python type-name tie -> no set).
    ({"a": 5}, {"$max": {"a": True}}, False),  # bool rank 9 > number -> set
    ({"a": True}, {"$min": {"a": 5}}, False),  # number rank 3 < bool -> set
    ({"a": False}, {"$max": {"a": True}}, False),  # False < True -> set
    ({"a": 3}, {"$min": {"a": bson.Decimal128("2.5")}}, False),  # 2.5 < 3 -> set
    ({"a": bson.Decimal128("2.5")}, {"$max": {"a": 3}}, False),  # 3 > 2.5 -> set
    # (A NaN-valued *field* also keeps its NaN on both engines, but the parity
    # assert can't equality-compare a NaN payload, so only the NaN-operand
    # direction is pinned here.)
    ({"a": 5}, {"$max": {"a": float("nan")}}, False),  # 5 < nan False -> keep
    ({"a": b"ab"}, {"$max": {"a": b"ac"}}, False),  # bytes compare -> set
    ({"a": bson.Timestamp(5, 1)}, {"$min": {"a": bson.Timestamp(4, 9)}}, False),
    ({"a": bson.Regex("a")}, {"$max": {"a": bson.Regex("b")}}, False),  # tie -> keep
    ({"a": True}, {"$max": {"a": 2}}, False),  # bool current -> Rust defers (skip)
    ({"a": "m"}, {"$min": {"a": "a"}}, False),  # string compare
    ({"a": 2}, {"$min": {"a": 1.5}}, False),  # int/float cross-numeric
    # Cross-type (sortable) now COMPUTES on both engines via BSON order:
    ({"a": 5}, {"$min": {"a": "x"}}, False),  # number < string -> keep 5
    ({"a": 5}, {"$max": {"a": "x"}}, False),  # string > number -> set "x"
    ({"a": ObjectId("507f1f77bcf86cd799439011")}, {"$max": {"a": 5}}, False),  # oid > num
    ({"a": "x"}, {"$max": {"a": 5}}, False),  # string > number -> keep "x"
    # $addToSet — dedup by value (bool-as-int, structural), absent -> create.
    ({"a": [1, 2]}, {"$addToSet": {"a": 3}}, False),  # append
    ({"a": [1, 2, 3]}, {"$addToSet": {"a": 2}}, False),  # present -> no change
    ({"a": [1, 2]}, {"$addToSet": {"a": True}}, False),  # True==1 present
    ({"a": [{"x": 1}]}, {"$addToSet": {"a": {"x": 1}}}, False),  # structural dup
    ({}, {"$addToSet": {"a": 7}}, False),  # absent -> [7]
    ({"a": 5}, {"$addToSet": {"a": 1}}, False),  # non-array -> defer (Python raises)
    # $pull — remove elements `==` the criterion (value compare, not query).
    ({"a": [1, 2, 3, 2]}, {"$pull": {"a": 2}}, False),
    ({"a": [{"x": 1}, {"x": 2}]}, {"$pull": {"a": {"x": 1}}}, False),  # sub-doc match
    ({"a": [1, True, 2]}, {"$pull": {"a": 1}}, False),  # query eq: bool != int (keeps True)
    ({"a": [1, 1.0, 2]}, {"$pull": {"a": 1}}, False),  # query eq: 1 == 1.0 (removes both)
    ({"a": 5}, {"$pull": {"a": 1}}, False),  # non-array -> no-op
    # $pull with a query predicate / sub-document criterion (via query::matches).
    ({"a": [1, 5, 10, 15]}, {"$pull": {"a": {"$gte": 10}}}, False),
    ({"a": [1, 2, 3, 4]}, {"$pull": {"a": {"$in": [2, 4]}}}, False),
    ({"a": [1, 2, 3]}, {"$pull": {"a": {"$lt": 3}}}, False),
    (
        {"a": [{"x": 1, "y": "a"}, {"x": 5, "y": "b"}, {"x": 9, "y": "c"}]},
        {"$pull": {"a": {"x": {"$gte": 5}}}},
        False,
    ),
    (
        {"a": [{"x": 1, "y": "a"}, {"x": 5, "y": "b"}]},
        {"$pull": {"a": {"y": "b"}}},
        False,
    ),
    ({"a": [{"b": {"c": 1}}, {"b": {"c": 2}}]}, {"$pull": {"a": {"b.c": 2}}}, False),
    # $pullAll — literal equality over a value list.
    ({"a": [1, 2, 3, 2, 1]}, {"$pullAll": {"a": [1, 2]}}, False),
    ({"a": [1, 2, 3]}, {"$pullAll": {"a": [9]}}, False),  # nothing removed
    ({"a": 5}, {"$pullAll": {"a": [1]}}, False),  # non-array field -> no-op
    # $push $sort — 1/-1 whole-element and {field: dir} sorts (BSON order).
    ({"a": [1, 2]}, {"$push": {"a": {"$each": [4, 3], "$sort": 1}}}, False),
    ({"a": [3, 1, 2]}, {"$push": {"a": {"$each": [], "$sort": -1}}}, False),
    # Whole-double ±1 direction (int or {field: dir}) is accepted (both engines).
    ({"a": [3, 1]}, {"$push": {"a": {"$each": [2], "$sort": 1.0}}}, False),
    (
        {"a": [{"s": 3}, {"s": 1}]},
        {"$push": {"a": {"$each": [{"s": 2}], "$sort": {"s": 1.0}}}},
        False,
    ),
    (
        {"a": [{"s": 3}, {"s": 1}]},
        {"$push": {"a": {"$each": [{"s": 2}], "$sort": {"s": 1}}}},
        False,
    ),
    (
        {"a": [{"s": 1}, {"s": 3}]},
        {"$push": {"a": {"$each": [{"s": 2}], "$sort": {"s": -1}}}},
        False,
    ),
    # $push $sort + $slice combined (position -> sort -> slice).
    ({"a": [5, 1, 3]}, {"$push": {"a": {"$each": [2, 4], "$sort": 1, "$slice": 3}}}, False),
    # $bit already handled above.
    # Cases the Rust path should defer (rust returns None -> skipped):
    ({"_id": 1}, {"_id": 2, "x": 9}, False),  # _id change -> error path
    ({}, {"$set": {"a": 1}, "b": 2}, False),  # mixing -> error path
    ({"a": [1]}, {"$set": {"a.$": 9}}, False),  # positional -> defer
]


@pytest.mark.parametrize("doc,update,upsert", CURATED)
def test_curated_parity(doc, update, upsert):
    doc = bson.decode(bson.encode(doc))
    update = bson.decode(bson.encode(update))
    rust = _rust_apply(doc, update, upsert)
    if rust is None:
        return  # fallback case — shim would run pure Python
    py = _pure.apply_update(doc, update, is_upsert=upsert)
    # The Rust update engine now follows mongod's numeric type promotion
    # (int32 < int64 < double < decimal128) exactly like the pure-Python engine,
    # so the BSON int32-vs-int64 subtype must match — compare values directly.
    assert bson.decode(rust) == py, f"rust={bson.decode(rust)} pure={py} update={update}"


def _rust_apply_with(doc, update, array_filters=None, positional_matches=None, is_upsert=False):
    return _rust.apply_update_with(
        bson.encode(doc),
        bson.encode(update),
        is_upsert,
        bson.encode({"f": list(array_filters or [])}),
        bson.encode(dict(positional_matches or {})),
    )


# (doc, update, array_filters, positional_matches) for the positional /
# arrayFilters path. The Rust `apply_update_with` must match Python's
# `apply_update(..., array_filters=, positional_matches=)` byte-for-byte.
ARRAY_FILTER_CASES = [
    # $[] — all elements
    ({"g": [1, 2, 3]}, {"$inc": {"g.$[]": 10}}, [], {}),
    ({"g": [1, 2, 3]}, {"$set": {"g.$[]": 0}}, [], {}),
    ({"g": [{"v": 1}, {"v": 2}]}, {"$set": {"g.$[].v": 9}}, [], {}),
    # $[ident] with arrayFilters
    ({"g": [1, 2, 3]}, {"$set": {"g.$[e]": 0}}, [{"e": {"$gte": 2}}], {}),
    ({"g": [1, 2, 3, 4]}, {"$inc": {"g.$[e]": 100}}, [{"e": {"$lt": 3}}], {}),
    (
        {"items": [{"score": 40}, {"score": 80}, {"score": 10}]},
        {"$set": {"items.$[e].score": 100}},
        [{"e.score": {"$lt": 50}}],
        {},
    ),
    # $ positional (resolution supplied)
    ({"g": [5, 6, 7]}, {"$set": {"g.$": 60}}, [], {"g": 1}),
    ({"g": [5, 6, 7]}, {"$inc": {"g.$": 1}}, [], {"g": 2}),
    # nested $[] then $[ident]
    (
        {"a": [{"b": [1, 2]}, {"b": [3, 4]}]},
        {"$set": {"a.$[].b.$[x]": 0}},
        [{"x": {"$gte": 3}}],
        {},
    ),
    # no-op: identifier matches nothing
    ({"g": [1, 2]}, {"$set": {"g.$[e]": 9}}, [{"e": {"$gt": 100}}], {}),
    # single identifier nested inside $and / $or resolves and applies
    (
        {"a": [{"g": 1, "h": 1}, {"g": 5, "h": 9}]},
        {"$set": {"a.$[x].g": 9}},
        [{"$and": [{"x.g": {"$gt": 3}}, {"x.h": {"$gt": 0}}]}],
        {},
    ),
    (
        {"a": [{"g": 1}, {"g": 5}]},
        {"$set": {"a.$[x].g": 9}},
        [{"$or": [{"x.g": {"$gt": 3}}, {"x.g": {"$lt": 0}}]}],
        {},
    ),
]


@pytest.mark.parametrize("doc,update,af,pos", ARRAY_FILTER_CASES)
def test_array_filter_parity(doc, update, af, pos):
    doc = bson.decode(bson.encode(doc))
    update = bson.decode(bson.encode(update))
    rust = _rust_apply_with(doc, update, af, pos)
    if rust is None:
        return  # fallback — Python handles it
    py = _pure.apply_update(doc, update, array_filters=list(af), positional_matches=dict(pos))
    assert bson.decode(rust) == py, f"rust={bson.decode(rust)} pure={py} update={update} af={af}"


def _rand_scalar(rng):
    return rng.choice(
        [
            rng.randint(-20, 20),
            Int64(rng.randint(-20, 20)),
            round(rng.uniform(-9, 9), 2),
            rng.choice(["a", "bb", "z"]),
            True,
            False,
            None,
            ObjectId(),
        ]
    )


def _rand_doc(rng):
    d = {}
    for f in ("a", "b", "n"):
        r = rng.random()
        if r < 0.2:
            continue
        elif r < 0.4:
            d[f] = [_rand_scalar(rng) for _ in range(rng.randint(0, 3))]
        elif r < 0.5:
            d[f] = {"x": _rand_scalar(rng)}
        else:
            d[f] = _rand_scalar(rng)
    if rng.random() < 0.5:
        d["_id"] = rng.randint(1, 5)
    return d


def _rand_update(rng):
    op = rng.choice(
        [
            "$set",
            "$unset",
            "$inc",
            "$mul",
            "$push",
            "$pop",
            "$rename",
            "$min",
            "$max",
            "$addToSet",
            "$pull",
            "$pullAll",
            "replace",
        ]
    )
    field = rng.choice(["a", "b", "n", "a.x", "b.0"])
    if op == "replace":
        return {k: _rand_scalar(rng) for k in rng.sample(["p", "q", "r"], rng.randint(0, 3))}
    if op == "$set":
        return {op: {field: _rand_scalar(rng)}}
    if op == "$unset":
        return {op: {field: ""}}
    if op in ("$inc", "$mul"):
        return {op: {field: rng.choice([rng.randint(-5, 5), round(rng.uniform(-3, 3), 2)])}}
    if op in ("$push", "$addToSet") and rng.random() < 0.4:
        # $each modifier form (with occasional $position / $slice / $sort).
        val = {"$each": [_rand_scalar(rng) for _ in range(rng.randint(0, 3))]}
        if op == "$push":
            if rng.random() < 0.4:
                val["$position"] = rng.randint(-3, 4)
            if rng.random() < 0.4:
                val["$slice"] = rng.randint(-3, 4)
            if rng.random() < 0.2:
                val["$sort"] = rng.choice([1, -1])
        return {op: {field: val}}
    if op == "$pull" and rng.random() < 0.4:
        # $pull with a query predicate criterion.
        pred = rng.choice(
            [{"$gte": rng.randint(-3, 3)}, {"$lt": rng.randint(-3, 3)}, {"$in": [1, 2, 3]}]
        )
        return {op: {field: pred}}
    if op == "$pullAll":
        return {op: {field: [_rand_scalar(rng) for _ in range(rng.randint(0, 3))]}}
    if op in ("$push", "$min", "$max", "$addToSet", "$pull"):
        return {op: {field: _rand_scalar(rng)}}
    if op == "$pop":
        return {op: {field: rng.choice([1, -1])}}
    return {op: {field: rng.choice(["a", "b", "n", "c2"])}}  # $rename


def test_randomised_fuzz_parity():
    rng = random.Random(0x09DA7E)
    handled = 0
    for _ in range(6000):
        doc = bson.decode(bson.encode(_rand_doc(rng)))
        update = bson.decode(bson.encode(_rand_update(rng)))
        upsert = rng.random() < 0.5
        rust = _rust_apply(doc, update, upsert)
        if rust is None:
            continue
        handled += 1
        py = _pure.apply_update(doc, update, is_upsert=upsert)
        assert bson.decode(rust) == py, (
            f"divergence: rust={bson.decode(rust)} pure={py} update={update} doc={doc}"
        )
    assert handled > 1000, f"expected many handled cases, only {handled}"


def _rust_apply_batch(docs, update, is_upsert=False):
    res = _rust.apply_update_batch(bson.encode({"d": list(docs)}), bson.encode(update), is_upsert)
    return None if res is None else bson.decode(res)["d"]


def test_batch_apply_parity():
    """The batched seam applies one update to N docs, matching per-doc results,
    and defers the whole batch iff any single doc would defer."""
    assert _rust_apply_batch([], {"$set": {"a": 1}}) == []

    rng = random.Random(0x09DA_BA7)
    handled = 0
    for _ in range(3000):
        docs = [bson.decode(bson.encode(_rand_doc(rng))) for _ in range(rng.randint(0, 6))]
        update = bson.decode(bson.encode(_rand_update(rng)))
        upsert = rng.random() < 0.5
        rust = _rust_apply_batch(docs, update, upsert)
        per_doc = [_rust_apply(d, update, upsert) for d in docs]
        if rust is None:
            assert any(r is None for r in per_doc) or not docs
            continue
        handled += 1
        py = [_pure.apply_update(d, update, is_upsert=upsert) for d in docs]
        assert rust == py, f"batch divergence: rust={rust} pure={py} update={update}"
    assert handled > 500, f"expected many handled batches, only {handled}"


@pytest.mark.parametrize(
    "doc,update",
    [
        ({"n": 5}, {"$pull": {"n": 1}}),
        ({"n": None}, {"$pull": {"n": 1}}),
        ({"n": 5}, {"$pullAll": {"n": [1]}}),
        ({"n": None}, {"$pullAll": {"n": [1]}}),
    ],
)
def test_pull_non_array_defers_and_raises(doc, update):
    # $pull / $pullAll on a present but non-array field: Rust defers (None), the
    # pure engine raises code 2. A missing field is a no-op both sides (covered
    # by the curated/fuzz parity), not here.
    doc = bson.decode(bson.encode(doc))
    update = bson.decode(bson.encode(update))
    assert _rust_apply(doc, update) is None
    with pytest.raises(_pure.UpdateError) as exc:
        _pure.apply_update(doc, update)
    assert exc.value.code == 2


@pytest.mark.parametrize(
    "spec",
    [2, -2, 1.5, "x", True, [1], {"s": 2}, {"s": True}, {"s": "x"}],
)
def test_push_sort_invalid_defers_and_raises(spec):
    # An invalid $push $sort spec: Rust defers (None), the pure engine raises
    # code 2. Valid ±1 (int or whole double) sorts are covered by the curated
    # parity corpus.
    doc = bson.decode(bson.encode({"a": [{"s": 3}, {"s": 1}]}))
    update = bson.decode(
        bson.encode({"u": {"$push": {"a": {"$each": [{"s": 2}], "$sort": spec}}}})
    )["u"]
    assert _rust_apply(doc, update) is None
    with pytest.raises(_pure.UpdateError) as exc:
        _pure.apply_update(doc, update)
    assert exc.value.code == 2


@pytest.mark.parametrize(
    "af,code",
    [
        ([{}], 9),  # empty filter
        ([{"1x": {"$gt": 0}}], 2),  # bad identifier (starts with a digit)
        ([{"X": {"$gt": 0}}], 2),  # bad identifier (uppercase start)
        ([{"x": {"$gt": 0}}, {"x": {"$lt": 9}}], 9),  # duplicate identifier
        ([{"x": {"$gt": 0}}, {"y": {"$gt": 0}}], 9),  # 'y' unused
        ([{"x": {"$gt": 0}, "y": {"$gt": 0}}], 9),  # two identifiers in one filter
        ([{"$and": [{"x": {"$gt": 0}}, {"y": {"$gt": 0}}]}], 9),  # two, nested
        ([{"$expr": {"$gt": ["$g", 0]}}], 224),  # $expr, no identifier
    ],
)
def test_array_filters_invalid_defers_and_raises(af, code):
    # Invalid arrayFilters: Rust defers (None) so the pure engine raises the
    # exact code. Valid arrayFilters still compute (covered by the fuzz corpus).
    doc = bson.decode(bson.encode({"a": [{"g": 1}, {"g": 5}]}))
    update = bson.decode(bson.encode({"u": {"$set": {"a.$[x].g": 9}}}))["u"]
    af = [bson.decode(bson.encode(f)) for f in af]
    assert _rust_apply_with(doc, update, af) is None
    with pytest.raises(_pure.UpdateError) as exc:
        _pure.apply_update(doc, update, array_filters=af)
    assert exc.value.code == code


# Decimal128 arithmetic: the Rust engine computes these natively now (it used
# to defer wholesale). Both halves matter — the digits *and* the quantum, since
# decimal128 is a non-normalized format where 5.00 and 5 are distinguishable.
DECIMAL_CASES = [
    # 34 significant digits survive (a 28-digit context used to eat six).
    ({"v": Decimal128("1.000000000000000000000000000000001")}, {"$inc": {"v": Decimal128("1")}}),
    # Quantum is preserved: min(e1,e2) for add, e1+e2 for multiply.
    ({"v": Decimal128("2.50")}, {"$inc": {"v": Decimal128("0.10")}}),
    ({"v": Decimal128("2.50")}, {"$mul": {"v": Decimal128("2")}}),
    ({"v": Decimal128("0.1")}, {"$mul": {"v": Decimal128("0.1")}}),
    # Rounds half-even past 34 digits.
    (
        {"v": Decimal128("1.234567890123456789012345678901234")},
        {"$mul": {"v": Decimal128("9.999")}},
    ),
    # Mixed widening — a double joins the decimal domain at 15 significant
    # digits for the *update* operators (the accumulators differ; see
    # tests/test_rust_aggregate_parity.py).
    ({"v": Decimal128("1.5")}, {"$inc": {"v": 3.0}}),
    ({"v": Decimal128("1.5")}, {"$inc": {"v": 0.1}}),
    ({"v": 3.0}, {"$inc": {"v": Decimal128("1.5")}}),
    ({"v": Decimal128("1")}, {"$inc": {"v": Int64(5)}}),
    ({"v": 7}, {"$mul": {"v": Decimal128("0.5")}}),
    # Signs, zeros, and wide exponents.
    ({"v": Decimal128("-2.5")}, {"$inc": {"v": Decimal128("2.5")}}),
    ({"v": Decimal128("0E+10")}, {"$inc": {"v": Decimal128("-7.56E-26")}}),
    ({"v": Decimal128("1.128797342904130E-13")}, {"$inc": {"v": Decimal128("0E+10")}}),
    ({"v": Decimal128("-0")}, {"$mul": {"v": Decimal128("3")}}),
]


@pytest.mark.parametrize("doc,update", DECIMAL_CASES, ids=[str(u) for _, u in DECIMAL_CASES])
def test_decimal_arithmetic_matches_pure_python(doc, update):
    doc = bson.decode(bson.encode(doc))
    update = bson.decode(bson.encode(update))
    raw = _rust_apply(doc, update)
    assert raw is not None, "Rust deferred; decimal arithmetic should be native"
    assert bson.decode(raw) == _pure.apply_update(dict(doc), update)
