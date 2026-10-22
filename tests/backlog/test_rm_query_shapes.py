"""Gap tests for the query-operator divergences found on 2026-10-22 by
probing the lines of `secantus-core/src/query.rs` that no test executed.

The line-coverage run (`docs-rust/coverage.md`) found ~480 such lines: the
argument checks of `$type`, `$mod`, `$size` and the bit operators, regex
options, `$where`, path components, `$elemMatch` / `$all` parsing and
`$expr` field paths. `tools/probes/query_shapes.py` holds the cases written
against them. Each test here holds the shapes of one root cause and asks
mongod live, comparing the matched `_id`s, or the error's code AND message.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import Binary, Decimal128, Int64, Regex
from bson.raw_bson import RawBSONDocument

from tests.backlog.conftest import mongo_answer

NAN = float("nan")
INF = float("inf")


def _raw(field: str, type_byte: bytes, payload: bytes) -> RawBSONDocument:
    """A one-field document holding a value pymongo cannot encode itself (a
    Symbol, `undefined`), as raw BSON."""
    body = type_byte + field.encode() + b"\x00" + payload
    return RawBSONDocument((len(body) + 5).to_bytes(4, "little") + body + b"\x00")


def _symbol(text: str) -> bytes:
    data = text.encode() + b"\x00"
    return len(data).to_bytes(4, "little") + data


def _symbol_doc(_id: int, text: str) -> RawBSONDocument:
    """`{_id: <_id>, a: Symbol(text)}`."""
    body = b"\x10_id\x00" + _id.to_bytes(4, "little") + b"\x0ea\x00" + _symbol(text)
    return RawBSONDocument((len(body) + 5).to_bytes(4, "little") + body + b"\x00")


def _nested(field: str, op: str, type_byte: bytes, payload: bytes) -> RawBSONDocument:
    """`{field: {op: <raw value>}}`."""
    inner = type_byte + op.encode() + b"\x00" + payload
    sub = (len(inner) + 5).to_bytes(4, "little") + inner + b"\x00"
    body = b"\x03" + field.encode() + b"\x00" + sub
    return RawBSONDocument((len(body) + 5).to_bytes(4, "little") + body + b"\x00")


def _undefined_list(field: str, op: str) -> RawBSONDocument:
    """`{field: {op: [undefined]}}`."""
    arr = b"\x060\x00"
    arrdoc = (len(arr) + 5).to_bytes(4, "little") + arr + b"\x00"
    return _nested(field, op, b"\x04", arrdoc)


def _answer(client: Any, filt: Any, docs: list, where: str) -> Any:
    coll = client.rm_query_shapes.c
    coll.drop()
    coll.insert_many(docs)
    if where == "$match":
        pipeline = [{"$addFields": {"__probe": 1}}, {"$match": filt}, {"$sort": {"_id": 1}}]
        run = lambda: [d["_id"] for d in coll.aggregate(pipeline)]  # noqa: E731
    else:
        run = lambda: [d["_id"] for d in coll.find(filt).sort("_id", 1)]  # noqa: E731
    got = mongo_answer(run)
    return got[1] if got[0] == "ok" else got


def _diverging(rust_mongo: Any, mongod: Any, docs: list, shapes: list[tuple]) -> list[str]:
    """The shapes whose answers differ, each with both answers. A shape is
    `(label, filter)` or `(label, filter, "$match")`."""
    out = []
    for shape in shapes:
        label, filt = shape[0], shape[1]
        where = shape[2] if len(shape) > 2 else "find"
        ours, theirs = _answer(rust_mongo, filt, docs, where), _answer(mongod, filt, docs, where)
        if ours != theirs:
            out.append(f"{label}: mongod {theirs!r}, rust {ours!r}")
    return out


NUMS = [
    {"_id": 1, "a": 5},
    {"_id": 2, "a": -5},
    {"_id": 3, "a": Int64(-(2**63))},
    {"_id": 4, "a": 5.7},
    {"_id": 5, "a": Decimal128("5.5")},
    {"_id": 6, "a": [3, 4]},
    {"_id": 7, "a": Binary(b"\x05\x00", 0)},
    {"_id": 8, "a": "x"},
    {"_id": 9, "a": Int64(2**62)},
]


@pytest.mark.backlog_fixed("RM-259")
def test_bit_positions(rust_mongo: Any, mongod: Any) -> None:
    """A bit-position list is int32 positions; a mask may be a whole
    Decimal128. Before: a position of 2^31 or more was not refused, and a
    large one sized a byte buffer to it (2^40 asked for 128 GiB)."""
    shapes = [
        ("position 2^31", {"a": {"$bitsAllSet": [0, Int64(2**31)]}}),
        ("position 2^40", {"a": {"$bitsAnyClear": [Int64(2**40)]}}),
        ("position int32 max", {"a": {"$bitsAnySet": [2**31 - 1]}}),
        ("position 1.5", {"a": {"$bitsAllSet": [1.5]}}),
        ("position NaN", {"a": {"$bitsAllSet": [NAN]}}),
        ("position -1", {"a": {"$bitsAllSet": [-1]}}),
        ("position -1.0", {"a": {"$bitsAllSet": [-1.0]}}),
        ("position -0.0", {"a": {"$bitsAllSet": [-0.0]}}),
        ("position string", {"a": {"$bitsAllSet": ["0"]}}),
        ("position bool", {"a": {"$bitsAllSet": [True]}}),
        ("position null", {"a": {"$bitsAllSet": [None]}}),
        ("position nested", {"a": {"$bitsAllSet": [[0]]}}),
        ("position Decimal128", {"a": {"$bitsAllSet": [Decimal128("2")]}}),
        ("position 63", {"a": {"$bitsAllSet": [63]}}),
        ("position 200", {"a": {"$bitsAnyClear": [200]}}),
        ("empty positions", {"a": {"$bitsAllSet": []}}),
        ("Decimal128 mask", {"a": {"$bitsAllSet": Decimal128("5")}}),
        ("Decimal128 5.0 mask", {"a": {"$bitsAllSet": Decimal128("5.0")}}),
        ("2^63 double mask", {"a": {"$bitsAllSet": 2.0**63}}),
    ]
    assert _diverging(rust_mongo, mongod, NUMS, shapes) == []


@pytest.mark.backlog_fixed("RM-260")
def test_mod_arguments(rust_mongo: Any, mongod: Any) -> None:
    """`$mod` checks its arguments in mongod's order and truncates both. Before:
    a fractional remainder was compared unrounded, a NaN or infinite one
    matched nothing instead of being refused, and `x % -1` on -2^63
    overflowed."""
    shapes = [
        ("double remainder", {"a": {"$mod": [2, 1.0]}}),
        ("fractional remainder", {"a": {"$mod": [2, 1.5]}}),
        ("negative fractional remainder", {"a": {"$mod": [2, -1.5]}}),
        ("Decimal128 remainder", {"a": {"$mod": [2, Decimal128("1.9")]}}),
        ("NaN remainder", {"a": {"$mod": [2, NAN]}}),
        ("inf remainder", {"a": {"$mod": [2, INF]}}),
        ("huge remainder", {"a": {"$mod": [2, 1e300]}}),
        ("NaN divisor", {"a": {"$mod": [NAN, 0]}}),
        ("inf divisor", {"a": {"$mod": [INF, 0]}}),
        ("2^63 double divisor", {"a": {"$mod": [2.0**63, 0]}}),
        ("Decimal128 NaN divisor", {"a": {"$mod": [Decimal128("NaN"), 0]}}),
        ("-1 divisor", {"a": {"$mod": [-1, 0]}}),
        ("-1.0 divisor", {"a": {"$mod": [-1.0, 0]}}),
        ("zero divisor", {"a": {"$mod": [0, 0]}}),
        ("bool divisor", {"a": {"$mod": [True, 0]}}),
        ("bool remainder", {"a": {"$mod": [2, True]}}),
        ("string remainder", {"a": {"$mod": [2, "1"]}}),
        ("null remainder", {"a": {"$mod": [2, None]}}),
        ("three elements", {"a": {"$mod": [2, 1, 3]}}),
        ("three elements bad divisor", {"a": {"$mod": ["x", 1, 3]}}),
        ("one element", {"a": {"$mod": [2]}}),
        ("not an array", {"a": {"$mod": 2}}),
        ("in $elemMatch", {"a": {"$elemMatch": {"$mod": [2, 1]}}}),
    ]
    assert _diverging(rust_mongo, mongod, NUMS, shapes) == []


@pytest.mark.backlog_fixed("RM-261")
def test_type_and_size_arguments(rust_mongo: Any, mongod: Any) -> None:
    """`$type` takes a whole Decimal128 code and renders a bad numeric one as
    `%g`; `$size` refuses a length outside int32 before its sign. Before: a
    Decimal128 code deferred, and a long length matched nothing."""
    shapes = [
        ("$type Decimal128 2", {"a": {"$type": Decimal128("2")}}),
        ("$type Decimal128 16", {"a": {"$type": Decimal128("16")}}),
        ("$type Decimal128 2E0", {"a": {"$type": Decimal128("2E0")}}),
        ("$type Decimal128 -1", {"a": {"$type": Decimal128("-1")}}),
        ("$type list with Decimal128", {"a": {"$type": [Decimal128("2"), "int"]}}),
        ("$type huge long code", {"a": {"$type": Int64(2**40)}}),
        ("$size 2^31", {"a": {"$size": Int64(2**31)}}),
        ("$size -2^63 double", {"a": {"$size": -(2.0**63)}}),
        ("$size 1e16", {"a": {"$size": 1e16}}),
        ("$size 2^63 double", {"a": {"$size": 2.0**63}}),
    ]
    assert _diverging(rust_mongo, mongod, NUMS, shapes) == []


REGEX_DOCS = [
    {"_id": 1, "a": "abc"},
    {"_id": 2, "a": "ABC"},
    {"_id": 3, "a": "x\nabc"},
    {"_id": 4, "a": 5},
]


@pytest.mark.backlog_fixed("RM-262")
def test_regex_options(rust_mongo: Any, mongod: Any) -> None:
    """Regex flags are `imsxu` only, `$options` is a string beside a
    `$regex`, a pattern holds no NUL, and a Symbol is matched as a string.
    Before: each of the refusals deferred, and a Symbol matched nothing."""
    shapes = [
        ("$options bad flag", {"a": {"$regex": "a", "$options": "c"}}),
        ("$options g", {"a": {"$regex": "a", "$options": "g"}}),
        ("$options non-string", {"a": {"$regex": "a", "$options": 1}}),
        ("$options null", {"a": {"$regex": "a", "$options": None}}),
        ("$options without $regex", {"a": {"$options": "i"}}),
        ("$not $options only", {"a": {"$not": {"$options": "i"}}}),
        ("Regex with flags plus $options", {"a": {"$regex": Regex("a", "i"), "$options": "m"}}),
        ("Regex without flags plus $options", {"a": {"$regex": Regex("a", ""), "$options": "i"}}),
        ("embedded NUL", {"a": {"$regex": "a\x00b"}}),
        ("bare Regex bad flag", {"a": Regex("a", "c")}),
        ("$in Regex bad flag", {"a": {"$in": [Regex("a", "c")]}}),
        ("$options u", {"a": {"$regex": "abc", "$options": "u"}}),
    ]
    assert _diverging(rust_mongo, mongod, REGEX_DOCS, shapes) == []
    shapes = [
        ("$regex over a Symbol", {"a": {"$regex": "^ab"}}),
        ("$eq Symbol over a number", _nested("a", "$eq", b"\x0e", _symbol("abc"))),
    ]
    docs = [_symbol_doc(1, "abc"), {"_id": 2, "a": 1}, {"_id": 3, "a": "abc"}]
    assert _diverging(rust_mongo, mongod, docs, shapes) == []


PATHS = [
    {"_id": 1, "a": [{"b": 1}, 5]},
    {"_id": 2, "a": [{"": {"b": 1}}]},
    {"_id": 3, "a": {"": {"b": 1}}},
    {"_id": 4, "a": [1, 2]},
    {"_id": 5, "a": [[1]]},
    {"_id": 6, "a": {"0": 7, "00": 8, "01": 9}},
    {"_id": 7, "a": [7, 8]},
]


@pytest.mark.backlog_fixed("RM-263")
def test_path_components(rust_mongo: Any, mongod: Any) -> None:
    """An array index is canonical (`0`, or digits without a leading zero),
    and an empty component re-reads an array only at the end of a path.
    Before: `a.00` read index 0, and `a.` over an array matched nothing."""
    shapes = [
        ("a.0", {"a.0": 7}),
        ("a.00", {"a.00": 8}),
        ("a.01", {"a.01": 9}),
        ("a.00 over an array", {"a.00": 7}),
        ("trailing dot", {"a.": 1}),
        ("trailing dot, whole array", {"a.": [1, 2]}),
        ("trailing dot, inner array", {"a.": [1]}),
        ("trailing dot $exists", {"a.": {"$exists": True}}),
        ("empty middle component", {"a..b": 1}),
        ("empty middle component $exists", {"a..b": {"$exists": True}}),
    ]
    assert _diverging(rust_mongo, mongod, PATHS, shapes) == []


EXPR_DOCS = [
    {"_id": 1, "a": [{"b": 1}, {"b": 2}, 5, [{"b": 3}], {"c": 1}], "arr": [10, 20]},
    {"_id": 2, "a": {"b": 1}},
    {"_id": 3},
    {"_id": 4, "x": None},
]


@pytest.mark.backlog_fixed("RM-264")
def test_expr_field_paths(rust_mongo: Any, mongod: Any) -> None:
    """In an expression, a path through an array reads the rest of the path
    from the DOCUMENT elements only, always giving an array, and a numeric
    component is a field name; a missing value is never `$in` a list. Before:
    the query engine's path walk answered, and a missing needle found null."""
    shapes = [
        ("path through an array", {"$expr": {"$eq": ["$a.b", [1, 2]]}}),
        ("missing field through an array", {"$expr": {"$eq": ["$a.zz", []]}}),
        ("$type of that", {"$expr": {"$eq": [{"$type": "$a.zz"}, "array"]}}),
        ("numeric component", {"$expr": {"$eq": ["$arr.0", []]}}),
        ("$in a missing needle", {"$expr": {"$in": ["$x", [None]]}}),
        ("$in a present needle", {"$expr": {"$in": ["$a.b", [[1, 2], 1]]}}),
        ("in $match", {"$expr": {"$eq": ["$a.b", [1, 2]]}}, "$match"),
    ]
    assert _diverging(rust_mongo, mongod, EXPR_DOCS, shapes) == []


ARRS = [
    {"_id": 1, "v": [[1, 2]]},
    {"_id": 2, "v": [[]]},
    {"_id": 3, "v": [[{"x": 1}]]},
    {"_id": 4, "v": [{"x": 1}, {"x": 2, "y": 3}]},
    {"_id": 5, "v": [1, 2, 3]},
    {"_id": 6, "v": [{"x": 1, "$b": 2}]},
]


@pytest.mark.backlog_fixed("RM-265")
def test_elem_match_all_and_operator_documents(rust_mongo: Any, mongod: Any) -> None:
    """A document is operators when its FIRST key is one; `$elemMatch`
    matches an array element as the document `{"0": ..., "1": ...}` and
    refuses the top-level-only operators; `$all` is decided by its first
    element. Before: an array element never matched a non-empty criteria,
    and the refusals deferred or matched."""
    shapes = [
        ("$elemMatch index of an inner array", {"v": {"$elemMatch": {"0": 1}}}),
        ("$elemMatch null field", {"v": {"$elemMatch": {"x": None}}}),
        ("$elemMatch $nor", {"v": {"$elemMatch": {"$nor": [{"x": 1}]}}}),
        ("$elemMatch dotted", {"v": {"$elemMatch": {"0.x": 1}}}),
        ("$elemMatch $and empty", {"v": {"$elemMatch": {"$and": []}}}),
        ("$elemMatch $or non-array", {"v": {"$elemMatch": {"$or": {"x": 1}}}}),
        ("$elemMatch $expr", {"v": {"$elemMatch": {"$expr": True}}}),
        ("$elemMatch $where", {"v": {"$elemMatch": {"$where": "1"}}}),
        ("$elemMatch operator then field", {"v": {"$elemMatch": {"$gt": 1, "x": 1}}}),
        ("operator then field", {"v": {"$gt": 1, "b": 2}}),
        ("$in document with a later $ key", {"v": {"$in": [{"x": 1, "$b": 2}]}}),
        ("$all $elemMatch then a value", {"v": {"$all": [{"$elemMatch": {"x": 1}}, 1]}}),
        ("$all $elemMatch non-document", {"v": {"$all": [{"$elemMatch": 1}]}}),
        ("$all another operator", {"v": {"$all": [{"$gt": 1}]}}),
    ]
    assert _diverging(rust_mongo, mongod, ARRS, shapes) == []


@pytest.mark.backlog_fixed("RM-266")
def test_operator_refusals(rust_mongo: Any, mongod: Any) -> None:
    """The operators mongod refuses by name or argument, `$sampleRate`, and
    `undefined` as an operand. Before: each deferred to "query uses a
    construct the Rust server does not support", or (`undefined` under a
    range operator) matched nothing."""
    shapes = [
        ("$comment as a field operator", {"a": {"$comment": "x"}}),
        ("$maxDistance alone", {"a": {"$maxDistance": 1}}),
        ("$not $comment", {"a": {"$not": {"$comment": "x"}}}),
        ("$where number", {"$where": 1}),
        ("$alwaysTrue Decimal128", {"$alwaysTrue": Decimal128("1")}),
        ("$sampleRate 1", {"$sampleRate": 1.0}),
        ("$sampleRate 0", {"$sampleRate": Int64(0)}),
        ("$sampleRate string", {"$sampleRate": "x"}),
        ("$sampleRate above 1", {"$sampleRate": 1.5}),
        ("$sampleRate NaN", {"$sampleRate": NAN}),
        ("$sampleRate as a field operator", {"a": {"$sampleRate": 1}}),
        ("bare undefined", _raw("a", b"\x06", b"")),
        ("$eq undefined", _nested("a", "$eq", b"\x06", b"")),
        ("$gt undefined", _nested("a", "$gt", b"\x06", b"")),
        ("$in [undefined]", _undefined_list("a", "$in")),
        ("$all [undefined]", _undefined_list("a", "$all")),
    ]
    assert _diverging(rust_mongo, mongod, NUMS, shapes) == []


@pytest.mark.backlog_fixed("RM-267")
def test_match_stage_parse_errors(rust_mongo: Any, mongod: Any) -> None:
    """A `$match` filter's parse error is sent bare, as `find` sends it, and
    `$text` may only be in the first `$match`. Before: the error carried
    "Executor error during aggregate command", and a later `$text` answered
    "not supported"."""
    shapes = [
        ("$and object", {"$and": {"a": 1}}, "$match"),
        ("$or non-document entry", {"$or": [1]}, "$match"),
        ("$nor scalar", {"$nor": 1}, "$match"),
        ("unknown operator", {"a": {"$foo": 1}}, "$match"),
        ("$text after a stage", {"$text": {"$search": "abc"}}, "$match"),
        ("$text under $or after a stage", {"$or": [{"$text": {"$search": "abc"}}]}, "$match"),
    ]
    assert _diverging(rust_mongo, mongod, NUMS, shapes) == []
