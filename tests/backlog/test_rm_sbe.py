"""mongod's slot-based engine (SBE), against the Rust server (RM-280).

mongod 8.2 runs the front of a pipeline in SBE when it can push a `$group` or
`$lookup` down -- the find layer's `$match`, `$sort`, `$skip` / `$limit` and
one inclusion or computed `$project`, then each SBE-compatible `$group` /
`$lookup` -- and SBE answers its own way: other error codes and messages,
some other values, its own tie order in the N-accumulators, and checks made
while it builds the plan, before any document is read. Each test runs the
same pipelines on mongod live and on the Rust server and compares the
answers as canonical extended JSON, or the error's code and message. Measured
against mongod 8.2.11, 2026-10-23, with `tools/probes/expression_shapes.py
--sbe` and its `expression_shapes_sbe.py` corpus.
"""

from __future__ import annotations

import datetime
import random
import sys
from typing import Any

import pytest
from bson import Decimal128, Int64, Regex, Timestamp, json_util
from bson.json_util import JSONMode, JSONOptions

from tests.backlog.conftest import mongo_answer

_CANON = JSONOptions(json_mode=JSONMode.CANONICAL)
#: A stage mongod cannot push into SBE: what follows it runs classic.
_CLASSIC = [{"$addFields": {"zz": 1}}]
_DB = "rm_sbe"


def _answer(client: Any, pipeline: list, docs: list, coll: str = "c") -> Any:
    db = client[_DB]
    db[coll].drop()
    db.create_collection(coll)
    if docs:
        db[coll].insert_many(docs)
    return mongo_answer(
        lambda: [json_util.dumps(d, json_options=_CANON) for d in db[coll].aggregate(pipeline)]
    )


def _diverging(rust_mongo: Any, mongod: Any, shapes: list) -> list:
    out = []
    for label, pipeline, docs in shapes:
        ours = _answer(rust_mongo, pipeline, docs)
        theirs = _answer(mongod, pipeline, docs)
        if ours != theirs:
            out.append(f"{label}:\n  mongod {theirs!r}\n  rust   {ours!r}")
    return out


def _sbe(expr: Any) -> list:
    """`expr` in a `$group` that leads the pipeline: SBE when it compiles."""
    return [{"$group": {"_id": None, "v": {"$first": expr}}}]


DOC = {
    "_id": 1,
    "n": 2,
    "z": 0,
    "d": 1.5,
    "s": "abc",
    "u": "héllo",
    "arr": [3, 1, 2],
    "o": {"k": 1},
    "t": datetime.datetime(2026, 1, 2, 3, 4, 5),
    "ts": Timestamp(5, 1),
    "dec": Decimal128("-1"),
    "re": Regex("a", "i"),
    "nul": None,
}


@pytest.mark.backlog_fixed("RM-280")
def test_pushdown_decides_the_engine(rust_mongo: Any, mongod: Any) -> None:
    """`$mod` by zero is 4848403 in SBE and 16610 in the classic engine, so
    it shows which engine ran each stage: the find layer and the chain of
    `$group` / `$lookup` after it are SBE; anything else, and a stage behind
    one that is not pushed, is classic. A `$unionWith` sub-pipeline is a
    query of its own and has its own pushdown; `$facet` and a `$lookup`
    sub-pipeline do not."""
    docs = [{"_id": 1, "a": 5, "z": 0, "k": 5}]
    mod = {"$mod": ["$a", "$z"]}
    grp = {"$group": {"_id": mod}}
    lookup = {"$lookup": {"from": "c", "localField": "a", "foreignField": "k", "as": "j"}}
    shapes = [
        ("leading $group", [grp], docs),
        ("$match then $group", [{"$match": {"a": 5}}, grp], docs),
        (
            "$sort, inclusion $project",
            [{"$sort": {"a": 1}}, {"$project": {"a": 1, "z": 1}}, grp],
            docs,
        ),
        ("$skip / $limit", [{"$skip": 0}, {"$limit": 5}, grp], docs),
        ("$addFields first", [*_CLASSIC, grp], docs),
        ("exclusion $project", [{"$project": {"k": 0}}, grp], docs),
        ("$project then $sort", [{"$project": {"a": 1, "z": 1}}, {"$sort": {"a": 1}}, grp], docs),
        ("$unwind first", [{"$unwind": "$arr"}, grp], docs),
        (
            "incompatible $group",
            [{"$group": {"_id": {"$toInt": "$a"}, "m": {"$first": mod}}}],
            docs,
        ),
        (
            "$expr in the filter",
            [{"$match": {"$expr": {"$eq": [mod, 1]}}}, {"$group": {"_id": 1}}],
            docs,
        ),
        ("$expr with nothing pushed", [{"$match": {"$expr": {"$eq": [mod, 1]}}}], docs),
        ("computed $project", [{"$project": {"q": mod}}, {"$group": {"_id": 1}}], docs),
        (
            "second $group",
            [
                {"$group": {"_id": "$a", "z": {"$first": "$z"}}},
                {"$group": {"_id": {"$mod": ["$_id", "$z"]}}},
            ],
            docs,
        ),
        (
            "$group after $sort after $group",
            [
                {"$group": {"_id": "$a", "z": {"$first": "$z"}}},
                {"$sort": {"_id": 1}},
                {"$group": {"_id": {"$mod": ["$_id", "$z"]}}},
            ],
            docs,
        ),
        ("$lookup then $group", [lookup, grp], docs),
        ("$lookup with $unwind", [lookup, {"$unwind": "$j"}, grp], docs),
        (
            "$unionWith sub-pipeline",
            [{"$limit": 0}, {"$unionWith": {"coll": "c", "pipeline": [grp]}}],
            docs,
        ),
        ("$facet sub-pipeline", [{"$facet": {"f": [grp]}}], docs),
        ("$lookup sub-pipeline", [{"$lookup": {"from": "c", "as": "j", "pipeline": [grp]}}], docs),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


#: One or more shapes per operator SBE compiles, each failing (or answering)
#: differently from the classic engine.
OPERATOR_SHAPES = [
    ("$abs", {"$abs": "$s"}),
    ("$abs long min", {"$abs": "$x"}, {"_id": 1, "x": Int64(-(2**63))}),
    ("$ceil", {"$ceil": "$s"}),
    ("$exp", {"$exp": "$s"}),
    ("$ln", {"$ln": "$dec"}),
    ("$log10 NaN", {"$log10": "$x"}, {"_id": 1, "x": Decimal128("NaN")}),
    ("$sqrt", {"$sqrt": "$dec"}),
    ("$mod by zero", {"$mod": ["$n", "$z"]}),
    ("$mod types", {"$mod": ["$s", "$n"]}),
    ("$divide by zero", {"$divide": ["$n", "$z"]}),
    ("$pow", {"$pow": ["$z", -1]}),
    ("$round place", {"$round": ["$d", "$d"]}),
    ("$round long", {"$round": ["$x", -1]}, {"_id": 1, "x": Int64(2**63 - 1)}),
    ("$trunc type", {"$trunc": ["$s", 0]}),
    ("$sin", {"$sin": "$x"}, {"_id": 1, "x": Decimal128("-Infinity")}),
    ("$acosh", {"$acosh": "$z"}),
    ("$atan type", {"$atan": "$s"}),
    ("$substrBytes fraction", {"$substrBytes": ["$s", "$d", 1]}),
    ("$substrBytes start", {"$substrBytes": ["$s", -1, 1]}),
    ("$substr timestamp", {"$substr": ["$ts", 0, 20]}),
    ("$substrCP start", {"$substrCP": ["$s", True, 1]}),
    ("$substrCP timestamp", {"$substrCP": ["$ts", 0, 3]}),
    ("$toUpper array", {"$toUpper": "$arr"}),
    ("$strcasecmp array", {"$strcasecmp": ["$arr", "a"]}),
    ("$indexOfCP start", {"$indexOfCP": ["$s", "b", "$d"]}),
    ("$indexOfBytes end", {"$indexOfBytes": ["$s", "b", 0, -1]}),
    ("$concat", {"$concat": ["$s", "$n"]}),
    ("$strLenCP", {"$strLenCP": "$n"}),
    ("$regexMatch input", {"$regexMatch": {"input": 5, "regex": "5"}}),
    ("$regexMatch no pattern", {"$regexMatch": {"input": "abc", "regex": "$nope"}}),
    ("$regexFindAll options", {"$regexFindAll": {"input": "$s", "regex": "$re", "options": "m"}}),
    (
        "$regexFind bad pattern",
        {"$regexFind": {"input": "$s", "regex": "$x"}},
        {"_id": 1, "s": "a", "x": "("},
    ),
    ("$arrayToObject v first", {"$arrayToObject": "$x"}, {"_id": 1, "x": [{"v": 1, "k": "a"}]}),
    ("$arrayToObject pair", {"$arrayToObject": "$x"}, {"_id": 1, "x": [["a"]]}),
    ("$objectToArray", {"$objectToArray": "$n"}),
    ("$bsonSize", {"$bsonSize": "$n"}),
    ("$anyElementTrue", {"$anyElementTrue": "$n"}),
    ("$concatArrays", {"$concatArrays": ["$arr", "$n"]}),
    ("$switch", {"$switch": {"branches": [{"case": {"$gt": ["$n", 5]}, "then": 1}]}}),
    ("$subtract", {"$subtract": ["$n", "$t"]}),
    ("$add", {"$add": ["$t", "$t"]}),
    ("$multiply", {"$multiply": ["$n", "$s"]}),
    ("$range missing", {"$range": [0, "$nope"]}),
    ("$range step", {"$range": [0, 5, "$z"]}),
    ("$replaceOne", {"$replaceOne": {"input": "$n", "find": "a", "replacement": "b"}}),
    ("$sortArray", {"$sortArray": {"input": "$s", "sortBy": 1}}),
    ("$reverseArray", {"$reverseArray": "$n"}),
    ("$setUnion", {"$setUnion": ["$arr", "$n"]}),
    ("$setEquals null", {"$setEquals": ["$arr", "$nul"]}),
    ("$in", {"$in": [1, "$n"]}),
    ("$trim", {"$trim": {"input": "$n"}}),
    ("$tsSecond", {"$tsSecond": "$n"}),
    ("$year", {"$year": "$s"}),
    ("$year timezone", {"$year": {"date": "$t", "timezone": "$n"}}),
    ("$dateAdd amount", {"$dateAdd": {"startDate": "$t", "unit": "day", "amount": "$d"}}),
    ("$dateSubtract start", {"$dateSubtract": {"startDate": "$n", "unit": "day", "amount": 1}}),
    ("$dateDiff unit", {"$dateDiff": {"startDate": "$t", "endDate": "$t", "unit": "$n"}}),
    ("$dateToParts iso8601", {"$dateToParts": {"date": "$t", "iso8601": "$n"}}),
    ("$dateToString format", {"$dateToString": {"date": "$t", "format": "$n"}}),
    ("$dateToString constant format", {"$dateToString": {"date": "$t", "format": "%Q"}}),
    ("$dateFromString", {"$dateFromString": {"dateString": "$n"}}),
    (
        "$dateFromString timezone",
        {"$dateFromString": {"dateString": "2026-01-02", "timezone": "$n"}},
    ),
    ("$dateFromParts", {"$dateFromParts": {"year": "$d"}}),
    ("$dateFromParts missing timezone", {"$dateFromParts": {"year": 2020, "timezone": "$nope"}}),
    ("$dateTrunc unit", {"$dateTrunc": {"date": "$t", "unit": "$n"}}),
]


@pytest.mark.backlog_fixed("RM-280")
def test_operators_answer_as_sbe(rust_mongo: Any, mongod: Any) -> None:
    """Each operator SBE compiles makes SBE's checks, in SBE's order, with
    SBE's code and message, and computes SBE's value: a non-integral
    `$substrBytes` bound is null (classic rounds it), a missing regex null
    (classic false), `{v, k}` an error (classic reads it), a missing
    `$range` operand null (classic fails), a Timestamp the string
    `Timestamp(5, 1)` (classic a date)."""
    shapes = [
        (case[0], _sbe(case[1]), [case[2] if len(case) > 2 else DOC]) for case in OPERATOR_SHAPES
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-280")
def test_operators_stay_classic_when_not_pushed(rust_mongo: Any, mongod: Any) -> None:
    """The same shapes behind a stage mongod does not push down keep the
    classic engine's answers."""
    shapes = [
        (case[0], [*_CLASSIC, *_sbe(case[1])], [case[2] if len(case) > 2 else DOC])
        for case in OPERATOR_SHAPES
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


def _g(**accs: Any) -> list:
    return [{"$group": {"_id": None, **accs}}]


def _vals(values: list) -> list:
    return [{"_id": i, "x": v} for i, v in enumerate(values)]


@pytest.mark.backlog_fixed("RM-280")
def test_group_accumulator_codes(rust_mongo: Any, mongod: Any) -> None:
    """A `$group` mongod pushes into SBE fails with SBE's codes: 7548606 for
    a constant `n` (no executor wrapper) and 7548607 for a computed one,
    5911200 for `$mergeObjects`, 7157705 for `$ln` in the key."""
    docs = _vals([1, 2])
    shapes = []
    for label, g in [
        ("n zero", _g(t={"$firstN": {"n": 0, "input": "$x"}})),
        ("n fraction", _g(t={"$maxN": {"n": 2.5, "input": "$x"}})),
        ("n string", _g(t={"$topN": {"n": "a", "output": "$x", "sortBy": {"x": 1}}})),
        ("n field", _g(t={"$lastN": {"n": "$x", "input": "$x"}})),
        ("$mergeObjects", _g(m={"$mergeObjects": "$x"})),
        ("$ln key", [{"$group": {"_id": {"$ln": "abc"}}}]),
    ]:
        shapes.append((label, g, docs))
        shapes.append((f"{label} classic", [*_CLASSIC, *g], docs))
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-280")
def test_accumulators_answer_as_sbe(rust_mongo: Any, mongod: Any) -> None:
    """Of values that compare equal, SBE's `$min` / `$max` keep the LATER
    one; SBE's `$stdDevPop` steps in doubles with no fused multiply-add;
    SBE keeps `$maxN` ties in the order they came (with no value dropped,
    which libc++ and libstdc++ agree on)."""
    ties = [1, 1.0, Int64(1)]
    shapes = []
    for label, g, docs in [
        ("$min ties", _g(m={"$min": "$x"}), _vals(ties)),
        ("$max ties", _g(m={"$max": "$x"}), _vals(ties)),
        ("$min zeros", _g(m={"$min": "$x"}), _vals([0.0, -0.0])),
        ("$maxN ties", _g(m={"$maxN": {"n": 3, "input": "$x"}}), _vals(ties)),
        ("$minN ties", _g(m={"$minN": {"n": 3, "input": "$x"}}), _vals(ties)),
        ("$stdDevPop", _g(m={"$stdDevPop": "$x"}), _vals([1, Int64(2), 2.5, Decimal128("0.1")])),
        ("$stdDevSamp", _g(m={"$stdDevSamp": "$x"}), _vals([1.5, 2.25, 7.125, 3])),
    ]:
        shapes.append((label, g, docs))
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-280")
def test_built_before_any_document(rust_mongo: Any, mongod: Any) -> None:
    """SBE checks a constant `n`, a constant null timezone of the date-part
    operators and a constant `$dateToString` / `$dateFromString` format while
    it builds the plan, so an existing empty collection fails too, with no
    executor wrapper. A collection that does not exist is never built."""
    shapes = []
    for label, g in [
        ("n", _g(t={"$firstN": {"n": 0, "input": "$x"}})),
        ("$year timezone null", [{"$group": {"_id": {"$year": {"date": "$t", "timezone": None}}}}]),
        (
            "$dateToString format",
            [{"$group": {"_id": {"$dateToString": {"date": "$t", "format": "%Q"}}}}],
        ),
        (
            "$dateToString format type",
            [{"$group": {"_id": {"$dateToString": {"date": "$t", "format": 5}}}}],
        ),
        (
            "$dateFromString format",
            [{"$group": {"_id": {"$dateFromString": {"dateString": "$t", "format": "%Q"}}}}],
        ),
        (
            "$dateFromString format type",
            [{"$group": {"_id": {"$dateFromString": {"dateString": "$t", "format": 5}}}}],
        ),
    ]:
        shapes.append((f"{label}, empty collection", g, []))
        shapes.append((f"{label}, classic", [*_CLASSIC, *g], []))
    assert _diverging(rust_mongo, mongod, shapes) == []
    for client_name, client in (("rust", rust_mongo), ("mongod", mongod)):
        client[_DB].missing.drop()
        got = mongo_answer(
            lambda c=client: list(
                c[_DB].missing.aggregate(_g(t={"$firstN": {"n": 0, "input": "$x"}}))
            )
        )
        assert got == ("ok", []), client_name


def _push_up(a: list, hole: int, top: int, value: Any, less: Any) -> None:
    parent = (hole - 1) // 2
    while hole > top and less(a[parent], value):
        a[hole] = a[parent]
        hole = parent
        parent = (hole - 1) // 2
    a[hole] = value


def _adjust(a: list, hole: int, length: int, value: Any, less: Any) -> None:
    top = second = hole
    while second < (length - 1) // 2:
        second = 2 * (second + 1)
        if less(a[second], a[second - 1]):
            second -= 1
        a[hole] = a[second]
        hole = second
    if length % 2 == 0 and second == (length - 2) // 2:
        second = 2 * (second + 1)
        a[hole] = a[second - 1]
        hole = second - 1
    _push_up(a, hole, top, value, less)


def _heap_keep(items: list, n: int, worse_first: Any, final: Any) -> list:
    """libstdc++'s `push_heap` / `pop_heap` as SBE drives them, then its
    `std::sort`, which for 16 elements or fewer is an insertion sort."""
    heap: list = []
    for it in items:
        if len(heap) < n:
            heap.append(it)
            _push_up(heap, len(heap) - 1, 0, heap[-1], worse_first)
        elif worse_first(it, heap[0]):
            last = len(heap) - 1
            value = heap[last]
            heap[last] = heap[0]
            _adjust(heap, 0, last, value, worse_first)
            heap[-1] = it
            _push_up(heap, last, 0, it, worse_first)
    for i in range(1, len(heap)):
        value, j = heap[i], i
        while j > 0 and final(value, heap[j - 1]):
            heap[j] = heap[j - 1]
            j -= 1
        heap[j] = value
    return heap


def _model(acc: dict, docs: list) -> list:
    """SBE's answer on the Linux build, for an accumulator below."""
    ((op, spec),) = acc.items()
    n = spec["n"]
    if op in ("$maxN", "$minN"):
        sign = 1 if op == "$maxN" else -1
        better = lambda a, b: sign * (float(a) - float(b)) > 0  # noqa: E731
        return _heap_keep([d["v"] for d in docs], n, better, better)
    top = op == "$topN"
    items = [(d["k"], d["o"]) for d in docs]
    worse = (lambda a, b: a[0] < b[0]) if top else (lambda a, b: a[0] > b[0])  # noqa: E731
    return [o for _, o in _heap_keep(items, n, worse, lambda a, b: a[0] < b[0])]


@pytest.mark.backlog_fixed("RM-280")
def test_n_accumulator_ties_follow_the_heap(rust_mongo: Any, mongod: Any) -> None:
    """SBE keeps `$topN`, `$bottomN`, `$maxN` and `$minN` in a binary heap and
    sorts it with `std::sort`, so documents that sort equal survive and come
    out in the order those algorithms leave them. Which ones is the C++
    library's: libstdc++ on the Linux build, mongod's exemplar, which is
    compared live; the macOS build's libc++ breaks ties its own way (a libc++
    model reproduced the macOS mongod 8.2.11 on 1,500 of 1,500 cases,
    2026-10-23), so there the answer is compared with libstdc++'s algorithms
    instead, for heaps small enough that `std::sort` is an insertion sort."""
    linux = sys.platform.startswith("linux")
    rnd = random.Random(7)
    shapes, wrong = [], []
    for trial in range(12):
        size = rnd.choice([5, 17, 25, 40])
        keys = [rnd.choice([1, 2, 3]) for _ in range(size)]
        vals = [rnd.choice([1, 1.0, Int64(1), 2, 2.0]) for _ in range(size)]
        docs = [{"_id": i, "k": keys[i], "o": f"d{i}", "v": vals[i]} for i in range(size)]
        for n in (2, 5, 20):
            for acc in (
                {"$topN": {"n": n, "output": "$o", "sortBy": {"k": 1}}},
                {"$bottomN": {"n": n, "output": "$o", "sortBy": {"k": 1}}},
                {"$maxN": {"n": n, "input": "$v"}},
                {"$minN": {"n": n, "input": "$v"}},
            ):
                if linux:
                    shapes.append((f"{trial} {acc}", _g(r=acc), docs))
                elif min(n, size) <= 16:
                    want = json_util.dumps(_model(acc, docs), json_options=_CANON)
                    got = _answer(rust_mongo, _g(r=acc), docs)
                    got_r = (
                        json_util.dumps(json_util.loads(got[1][0])["r"], json_options=_CANON)
                        if got[0] == "ok"
                        else got
                    )
                    if got_r != want:
                        wrong.append(f"{trial} {acc}:\n  libstdc++ {want}\n  rust      {got_r}")
    if linux:
        wrong = _diverging(rust_mongo, mongod, shapes)
    assert wrong == []


@pytest.mark.backlog_fixed("RM-288")
def test_bottom_keeps_the_first_of_equal_documents(rust_mongo: Any, mongod: Any) -> None:
    """`$bottom` / `$bottomN` over documents that sort equal keep the FIRST
    seen, in both engines: `$bottom` over 1, 1.0, 1L is the int, and the
    bottom two of 1, 1.0, 1L, 1.0L, "1" are 1.0 and "1" (measured 8.2.11,
    2026-10-23). This server kept the last."""
    shapes = []
    for label, g, docs in [
        (
            "$bottom",
            _g(r={"$bottom": {"output": "$x", "sortBy": {"x": 1}}}),
            _vals([1, 1.0, Int64(1)]),
        ),
        (
            "$bottomN",
            _g(r={"$bottomN": {"n": 2, "output": "$x", "sortBy": {"x": 1}}}),
            _vals([1, 1.0, Decimal128("1"), Int64(1), "1"]),
        ),
    ]:
        shapes.append((label, g, docs))
        shapes.append((f"{label} classic", [*_CLASSIC, *g], docs))
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-289")
def test_classic_ties_keep_the_first(rust_mongo: Any, mongod: Any) -> None:
    """The classic engine's `$min` / `$max` keep the FIRST of values that
    compare equal, and its `$maxN` lists equal values latest first: over
    1, 1.0, 1L, `$min` is the int and `$maxN` is [1L, 1.0, 1] (SBE answers the
    long and [1, 1.0, 1L]; measured 8.2.11, 2026-10-23). This server answered
    SBE's everywhere."""
    ties = _vals([1, 1.0, Int64(1)])
    shapes = [
        ("$min", [*_CLASSIC, *_g(m={"$min": "$x"})], ties),
        ("$max", [*_CLASSIC, *_g(m={"$max": "$x"})], ties),
        ("$min zeros", [*_CLASSIC, *_g(m={"$min": "$x"})], _vals([0.0, -0.0])),
        ("$maxN", [*_CLASSIC, *_g(m={"$maxN": {"n": 3, "input": "$x"}})], ties),
        (
            "$maxN full",
            [*_CLASSIC, *_g(m={"$maxN": {"n": 2, "input": "$x"}})],
            _vals([2, 1, 1.0, Int64(1)]),
        ),
        (
            "$minN full",
            [*_CLASSIC, *_g(m={"$minN": {"n": 2, "input": "$x"}})],
            _vals([0, 1, 1.0, Int64(1)]),
        ),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-290")
def test_constant_timezone_refused_before_any_document(rust_mongo: Any, mongod: Any) -> None:
    """mongod's optimizer refuses a constant timezone a date operator cannot
    use -- 40485 for an unknown zone, 40517 for one that is not a string --
    before it reads anything, so a collection that does not exist fails too
    (measured 8.2.11, 2026-10-23). This server found it only on a document."""
    out = []
    for label, expr in [
        ("$year", {"$year": {"date": "$t", "timezone": "Mars"}}),
        ("$year type", {"$year": {"date": "$t", "timezone": 5}}),
        ("$dateToString", {"$dateToString": {"date": "$t", "timezone": "Mars"}}),
        ("$dateFromString", {"$dateFromString": {"dateString": "$t", "timezone": "Mars"}}),
        (
            "$dateAdd",
            {"$dateAdd": {"startDate": "$t", "unit": "day", "amount": 1, "timezone": "Mars"}},
        ),
    ]:
        pipeline = [{"$addFields": {"z": expr}}]
        answers = []
        for client in (mongod, rust_mongo):
            client[_DB].missing.drop()
            answers.append(
                mongo_answer(lambda c=client, p=pipeline: list(c[_DB].missing.aggregate(p)))
            )
        if answers[0] != answers[1]:
            out.append(f"{label}:\n  mongod {answers[0]!r}\n  rust   {answers[1]!r}")
    assert out == []


def _missing_and_one(rust_mongo: Any, mongod: Any, exprs: list) -> list:
    """Each expression in a `$project` over a collection that does not exist
    and over one holding a single document, on both servers."""
    out = []
    one = {"_id": 1, "t": datetime.datetime(2020, 1, 1), "s": "x"}
    for label, expr in exprs:
        pipeline = [{"$project": {"_id": 0, "z": expr}}]
        for coll, docs in (("missing", []), ("one", [one])):
            answers = []
            for client in (mongod, rust_mongo):
                client[_DB][coll].drop()
                if docs:
                    client[_DB][coll].insert_many(docs)
                answers.append(
                    mongo_answer(lambda c=client[_DB][coll], p=pipeline: list(c.aggregate(p)))
                )
            if answers[0] != answers[1]:
                out.append(f"{label} ({coll}):\n  mongod {answers[0]!r}\n  rust   {answers[1]!r}")
    return out


@pytest.mark.backlog_fixed("RM-290")
def test_date_arguments_checked_in_the_optimizers_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod's optimizer reads a date operator's CONSTANT arguments in the
    order `unit`, `startOfWeek`, `timezone`, `binSize`, before any document:
    a bad one fails, and a null one makes the whole expression null, so
    nothing after it is checked (measured 8.2.11, 2026-10-23). This server
    checked `binSize` first and the timezone before `unit`, raised them only
    on a document, and answered a null `unit` beside a bad timezone with the
    timezone's error. A `$let` variable bound to a constant is a constant
    there; one read from the document hides it. `$dateSubtract`'s amount of
    -2^63 is found per document, after a null start (it was an optimizer
    error)."""
    trunc = {"date": "$t", "unit": "bogus", "startOfWeek": "x", "timezone": "Mars", "binSize": 0}
    exprs = []
    for null in trunc:
        if null != "date":
            exprs.append((f"$dateTrunc null {null}", {"$dateTrunc": {**trunc, null: None}}))
    diff = {"startDate": "$t", "endDate": "$t", "unit": "week", "startOfWeek": "x"}
    exprs += [
        ("$dateTrunc unit, binSize", {"$dateTrunc": {"date": "$t", "unit": "x", "binSize": 0}}),
        ("$dateTrunc binSize, tz", {"$dateTrunc": {**trunc, "unit": "day", "startOfWeek": None}}),
        ("$dateTrunc binSize", {"$dateTrunc": {"date": "$t", "unit": "day", "binSize": "a"}}),
        ("$dateTrunc unit type", {"$dateTrunc": {"date": "$t", "unit": 5}}),
        ("$dateDiff startOfWeek, tz", {"$dateDiff": {**diff, "timezone": "Mars"}}),
        ("$dateDiff tz", {"$dateDiff": {**diff, "startOfWeek": "mon", "timezone": 5}}),
        (
            "$dateAdd unit, tz",
            {"$dateAdd": {"startDate": "$t", "unit": "x", "amount": 1, "timezone": "Mars"}},
        ),
        (
            "$dateAdd null unit",
            {"$dateAdd": {"startDate": "$t", "unit": None, "amount": 1, "timezone": "Mars"}},
        ),
        (
            "$dateSubtract -2^63",
            {"$dateSubtract": {"startDate": "$t", "unit": "day", "amount": Int64(-(2**63))}},
        ),
        (
            "$dateSubtract -2^63 string start",
            {"$dateSubtract": {"startDate": "$s", "unit": "day", "amount": Int64(-(2**63))}},
        ),
        ("$dateToParts", {"$dateToParts": {"date": "$t", "timezone": "Mars"}}),
        ("$dateFromParts", {"$dateFromParts": {"year": "$t", "timezone": 5}}),
        ("in $add", {"$add": [1, {"$hour": {"date": "$t", "timezone": ""}}]}),
        (
            "$let",
            {"$let": {"vars": {"z": "Mars"}, "in": {"$year": {"date": "$t", "timezone": "$$z"}}}},
        ),
        (
            "$let beside a field",
            {
                "$let": {
                    "vars": {"z": "Mars", "w": "$s"},
                    "in": {"$year": {"date": "$t", "timezone": "$$z"}},
                }
            },
        ),
        (
            "$let shadowed",
            {
                "$let": {
                    "vars": {"z": "Mars"},
                    "in": {
                        "$map": {
                            "input": [],
                            "as": "z",
                            "in": {"$year": {"date": "$t", "timezone": "$$z"}},
                        }
                    },
                }
            },
        ),
        ("$literal zone", {"$year": {"date": "$t", "timezone": {"$literal": "Mars"}}}),
    ]
    assert _missing_and_one(rust_mongo, mongod, exprs) == []


@pytest.mark.backlog_fixed("RM-293")
def test_a_variable_bound_from_input_is_not_constant(rust_mongo: Any, mongod: Any) -> None:
    """An error raised through a variable mongod binds from the input --
    `$map`'s `$$this`, a `$let` variable read from a field -- is a per-document
    error, "Executor error during ..." (measured 8.2.11, 2026-10-23). This
    server counts every bound variable as a constant, so it answers "Failed
    to optimize pipeline", and in `find` raises it on an empty collection."""
    one = {"_id": 1, "t": datetime.datetime(2020, 1, 1), "s": "x"}
    year = {"$year": {"date": "$t", "timezone": "$$this"}}
    out = []
    for coll, docs in (("missing", []), ("one", [one])):
        answers = []
        for client in (mongod, rust_mongo):
            c = client[_DB][coll]
            c.drop()
            if docs:
                c.insert_many(docs)
            answers.append(
                [
                    mongo_answer(
                        lambda c=c: list(
                            c.aggregate(
                                [{"$project": {"z": {"$map": {"input": ["Mars"], "in": year}}}}]
                            )
                        )
                    ),
                    mongo_answer(
                        lambda c=c: list(
                            c.find(
                                {"$expr": {"$eq": [{"$map": {"input": ["Mars"], "in": year}}, 1]}}
                            )
                        )
                    ),
                    mongo_answer(
                        lambda c=c: list(
                            c.aggregate(
                                [
                                    {
                                        "$project": {
                                            "z": {
                                                "$let": {
                                                    "vars": {"z": "$s"},
                                                    "in": {
                                                        "$year": {"date": "$t", "timezone": "$$z"}
                                                    },
                                                }
                                            }
                                        }
                                    }
                                ]
                            )
                        )
                    ),
                ]
            )
        if answers[0] != answers[1]:
            out.append(f"{coll}:\n  mongod {answers[0]!r}\n  rust   {answers[1]!r}")
    assert out == []


@pytest.mark.backlog_fixed("RM-293")
def test_a_command_let_variable_is_folded_while_optimizing(rust_mongo: Any, mongod: Any) -> None:
    """mongod substitutes a command's `let`, `$$NOW` and a constant `$let`
    binding before it folds, so `{$divide: [1, "$$z"]}` with `let: {z: 0}`
    fails under the optimizer's wrapper before any document is read: in
    `aggregate` (in `$match`, `$project`, `$unionWith` and `$facet`), `find`
    (filter and projection) and `findAndModify`, while `update` and `delete`
    answer `n: 0` (measured 8.2.11, 2026-10-24). This server folded none of
    them, so each answered as though the collection were simply empty."""
    zero = {"$divide": [1, "$$z"]}
    now_zero = {"$subtract": [{"$toLong": "$$NOW"}, {"$toLong": "$$NOW"}]}
    expr = {"$expr": {"$eq": [zero, "$a"]}}
    proj = {"v": zero}
    let = {"z": 0, "o": {"z": 0}, "e": {"$subtract": [1, 1]}, "tz": "Mars"}
    year = {"$year": {"date": "$t", "timezone": "$$tz"}}
    commands = {
        "aggregate $match": {"aggregate": "c", "pipeline": [{"$match": expr}], "cursor": {}},
        "aggregate $project": {"aggregate": "c", "pipeline": [{"$project": proj}], "cursor": {}},
        "aggregate $unionWith": {
            "aggregate": "c",
            "pipeline": [{"$unionWith": {"coll": "c", "pipeline": [{"$project": proj}]}}],
            "cursor": {},
        },
        "aggregate $facet": {
            "aggregate": "c",
            "pipeline": [{"$facet": {"f": [{"$project": proj}]}}],
            "cursor": {},
        },
        "aggregate $group zone": {
            "aggregate": "c",
            "pipeline": [{"$group": {"_id": None, "v": {"$max": year}}}],
            "cursor": {},
        },
        "aggregate $$NOW": {
            "aggregate": "c",
            "pipeline": [{"$project": {"v": {"$divide": [1, now_zero]}}}],
            "cursor": {},
        },
        "aggregate $let": {
            "aggregate": "c",
            "pipeline": [
                {
                    "$project": {
                        "v": {
                            "$let": {
                                "vars": {"y": 0},
                                "in": {"$add": ["$a", {"$divide": [1, "$$y"]}]},
                            }
                        }
                    }
                }
            ],
            "cursor": {},
        },
        "aggregate let path": {
            "aggregate": "c",
            "pipeline": [{"$project": {"v": {"$add": ["$a", {"$divide": [1, "$$o.z"]}]}}}],
            "cursor": {},
        },
        "aggregate let expression": {
            "aggregate": "c",
            "pipeline": [{"$project": {"v": {"$add": ["$a", {"$divide": [1, "$$e"]}]}}}],
            "cursor": {},
        },
        "aggregate let in $map": {
            "aggregate": "c",
            "pipeline": [{"$project": {"v": {"$map": {"input": "$a", "in": zero}}}}],
            "cursor": {},
        },
        "find filter": {"find": "c", "filter": expr},
        "find filter zone": {"find": "c", "filter": {"$expr": {"$eq": [year, 1]}}},
        "find projection": {"find": "c", "projection": proj},
        "findAndModify": {"findAndModify": "c", "query": expr, "update": {"$set": {"b": 1}}},
        "update": {"update": "c", "updates": [{"q": expr, "u": {"$set": {"b": 1}}}]},
        "delete": {"delete": "c", "deletes": [{"q": expr, "limit": 0}]},
    }
    one = {"_id": 1, "t": datetime.datetime(2020, 1, 1), "a": 1}
    out = []
    for docs in ([], [one]):
        answers = []
        for client in (mongod, rust_mongo):
            db = client[_DB]
            db.c.drop()
            if docs:
                db.c.insert_many(docs)
            got = {}
            for label, cmd in commands.items():
                if docs and label.startswith(("update", "delete", "findAndModify")):
                    continue
                reply = mongo_answer(lambda db=db, cmd=cmd: db.command({**cmd, "let": let}))
                if reply[0] == "ok":
                    reply = ("ok", {k: reply[1][k] for k in ("n", "nModified") if k in reply[1]})
                got[label] = reply
            answers.append(got)
        for label in answers[0]:
            if answers[0][label] != answers[1][label]:
                out.append(
                    f"{label} ({len(docs)} docs):\n  mongod {answers[0][label]!r}\n"
                    f"  rust   {answers[1][label]!r}"
                )
    assert out == []


@pytest.mark.backlog_fixed("RM-296")
def test_find_projection_error_carries_the_executor_wrapper(rust_mongo: Any, mongod: Any) -> None:
    """An error a `find` projection raises on a document is "Executor error
    during find command: <ns> :: caused by :: ...", as in the filter, and
    `findAndModify`'s is "Plan executor error during findAndModify :: caused
    by :: ..." (measured 8.2.11, 2026-10-24); this server sent the bare
    message."""
    one = {"_id": 1, "t": datetime.datetime(2020, 1, 1), "s": "x"}
    projections = [
        {"v": {"$year": {"date": "$t", "timezone": "$s"}}},
        {"v": {"$map": {"input": ["Mars"], "in": {"$year": {"date": "$t", "timezone": "$$this"}}}}},
        {"v": {"$toUpper": "$t"}, "w": 1},
    ]
    answers = []
    for client in (mongod, rust_mongo):
        c = client[_DB]["one"]
        c.drop()
        c.insert_one(one)
        got = [mongo_answer(lambda c=c, p=p: list(c.find({}, p))) for p in projections]
        # findAndModify's wrapper is the plan executor's.
        got.append(
            mongo_answer(
                lambda c=c: c.find_one_and_update(
                    {},
                    {"$set": {"q": 1}},
                    projection={
                        "v": {
                            "$divide": [
                                1,
                                0,
                            ]
                        },
                        "w": "$s",
                    },
                )
            )
        )
        got.append(
            mongo_answer(
                lambda c=c: c.find_one_and_update(
                    {}, {"$set": {"q": 1}}, projection={"v": {"$toUpper": "$t"}}
                )
            )
        )
        answers.append(got)
    assert answers[1] == answers[0]


@pytest.mark.backlog_fixed("RM-297")
def test_find_projection_reads_the_command_let(rust_mongo: Any, mongod: Any) -> None:
    """A `find` (and `findAndModify`) projection may read the command's
    `let` and `$$NOW`: `{v: {$toUpper: "$$cz"}}` with `let: {cz: "Mars"}`
    gives `v: "MARS"`, and an undefined variable is 17276 (measured 8.2.11,
    2026-10-24). This server refused every one with code 2, "projection is
    not supported by the Rust server"."""
    let = {"cz": "Mars", "k": 10}
    answers = []
    for client in (mongod, rust_mongo):
        c = client[_DB]["one"]
        c.drop()
        c.insert_one({"_id": 1})
        answers.append(
            [
                mongo_answer(lambda c=c: list(c.find({}, {"v": {"$toUpper": "$$cz"}}, let=let))),
                mongo_answer(
                    lambda c=c: list(
                        c.find(
                            {},
                            {
                                "_id": 0,
                                "v": {"$map": {"input": [1], "in": {"$add": ["$$this", "$$k"]}}},
                            },
                            let=let,
                        )
                    )
                ),
                mongo_answer(lambda c=c: list(c.find({}, {"_id": 0, "v": "$$nope"}))),
                mongo_answer(
                    lambda c=c: [
                        type(d["v"]).__name__ for d in c.find({}, {"_id": 0, "v": "$$NOW"})
                    ]
                ),
                mongo_answer(
                    lambda c=c: c.find_one_and_update(
                        {},
                        {"$set": {"q": 1}},
                        projection={"_id": 0, "v": {"$toUpper": "$$cz"}},
                        let=let,
                    )
                ),
            ]
        )
    assert answers[1] == answers[0]


@pytest.mark.backlog_fixed("RM-291")
def test_classic_operator_checks_found_beside_sbe(rust_mongo: Any, mongod: Any) -> None:
    """Three classic answers found while comparing the two engines (measured
    8.2.11, 2026-10-23). A non-string `$replaceOne` / `$replaceAll` argument
    is 10503904 / 10503901 / 10503902 (this server answered "not
    supported"); a field operand of `$setEquals` that is no array is 17044
    at run time, and only a CONSTANT one is the optimizer's 5887502 (this
    server answered 5887502 for both), as a constant second operand of
    `$setIsSubset` is 17311; and a missing operand is named "missing", not
    "null", by `$range` and the set operators."""
    doc = [{"_id": 1, "arr": [1], "n": 2, "s": "abc", "nul": None, "o": {"a": 1}}]
    shapes = []
    for label, expr in [
        ("$replaceOne input", {"$replaceOne": {"input": "$n", "find": "a", "replacement": "b"}}),
        (
            "$replaceAll input doc",
            {"$replaceAll": {"input": "$o", "find": "a", "replacement": "b"}},
        ),
        ("$replaceOne find", {"$replaceOne": {"input": "$nope", "find": 1, "replacement": "b"}}),
        (
            "$replaceAll replacement",
            {"$replaceAll": {"input": "$s", "find": "a", "replacement": "$n"}},
        ),
        ("$setEquals field", {"$setEquals": ["$arr", "$nul"]}),
        ("$setEquals missing", {"$setEquals": ["$arr", "$nope"]}),
        ("$setEquals constant", {"$setEquals": ["$arr", 5]}),
        ("$setIsSubset constant", {"$setIsSubset": ["$arr", None]}),
        ("$setIsSubset missing", {"$setIsSubset": ["$arr", "$nope"]}),
        ("$range missing", {"$range": [0, "$nope"]}),
    ]:
        shapes.append((label, [*_CLASSIC, {"$project": {"z": expr}}], doc))
    assert _diverging(rust_mongo, mongod, shapes) == []
