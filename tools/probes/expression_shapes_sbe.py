"""Shapes for the operators mongod's slot-based engine compiles (RM-280).

`expression_shapes.py --sbe` runs every case in a `$group` mongod pushes into
SBE. Its own corpus was written for the classic engine's branches; these cases
reach the operators SBE compiles that it does not, each over a field of every
type, so that SBE's own checks (their order, codes and messages) and the values
its builtins compute are compared with mongod. Run with::

    cd tools/probes
    PROBE_SERVER=... python expression_shapes.py --sbe --cases expression_shapes_sbe.py
"""

from __future__ import annotations

import datetime

from bson import Decimal128, Int64, Regex

JAN1 = datetime.datetime(2020, 1, 1)

#: Fields of `expression_shapes.DOC`, one of each type, plus a missing one.
FIELDS = [
    "$n",
    "$d",
    "$s",
    "$u",
    "$arr",
    "$o",
    "$t",
    "$oid",
    "$dec",
    "$big",
    "$bin",
    "$ts",
    "$re",
    "$lo",
    "$nul",
    "$nope",
]

UNARY = [
    "$abs",
    "$ceil",
    "$floor",
    "$exp",
    "$sqrt",
    "$ln",
    "$log10",
    "$atan",
    "$asinh",
    "$cosh",
    "$sinh",
    "$tanh",
    "$degreesToRadians",
    "$radiansToDegrees",
    "$sin",
    "$acos",
    "$acosh",
    "$toLower",
    "$toUpper",
    "$strLenBytes",
    "$strLenCP",
    "$objectToArray",
    "$bsonSize",
    "$anyElementTrue",
    "$reverseArray",
    "$isArray",
    "$isNumber",
    "$not",
    "$tsSecond",
    "$tsIncrement",
    "$year",
    "$isoWeek",
    "$millisecond",
    "$arrayToObject",
]

BINARY = [
    "$add",
    "$subtract",
    "$multiply",
    "$divide",
    "$mod",
    "$pow",
    "$atan2",
    "$concat",
    "$concatArrays",
    "$setUnion",
    "$setIntersection",
    "$setDifference",
    "$setEquals",
    "$setIsSubset",
    "$in",
    "$cmp",
    "$eq",
    "$lt",
    "$strcasecmp",
    "$and",
    "$or",
    "$ifNull",
]

CASES: list[tuple] = []
for op in UNARY:
    for f in FIELDS:
        CASES.append((f"sbe {op} {f}", {op: f}))
for op in BINARY:
    for a in FIELDS:
        for b in ("$n", "$s", "$arr", "$nul", "$nope", "$d", "$t"):
            CASES.append((f"sbe {op} {a} {b}", {op: [a, b]}))

for f in FIELDS:
    CASES += [
        (f"sbe $trim {f}", {"$trim": {"input": f}}),
        (f"sbe $trim chars {f}", {"$trim": {"input": "$s", "chars": f}}),
        (f"sbe $ltrim {f}", {"$ltrim": {"input": f}}),
        (f"sbe $rtrim chars {f}", {"$rtrim": {"input": "$s", "chars": f}}),
        (
            f"sbe $replaceOne input {f}",
            {"$replaceOne": {"input": f, "find": "b", "replacement": "x"}},
        ),
        (
            f"sbe $replaceOne find {f}",
            {"$replaceOne": {"input": "$s", "find": f, "replacement": "x"}},
        ),
        (
            f"sbe $replaceOne replacement {f}",
            {"$replaceOne": {"input": "$s", "find": "b", "replacement": f}},
        ),
        (f"sbe $range start {f}", {"$range": [f, 5]}),
        (f"sbe $range end {f}", {"$range": [0, f]}),
        (f"sbe $range step {f}", {"$range": [0, 5, f]}),
        (f"sbe $round place {f}", {"$round": ["$d", f]}),
        (f"sbe $substrCP {f}", {"$substrCP": [f, 0, 2]}),
        (f"sbe $indexOfCP {f}", {"$indexOfCP": [f, "b"]}),
        (f"sbe $indexOfBytes sub {f}", {"$indexOfBytes": ["$s", f]}),
        (f"sbe $sortArray {f}", {"$sortArray": {"input": f, "sortBy": 1}}),
        (f"sbe $dateToString {f}", {"$dateToString": {"date": f}}),
        (f"sbe $dateToString tz {f}", {"$dateToString": {"date": "$t", "timezone": f}}),
        (f"sbe $dateToParts {f}", {"$dateToParts": {"date": f}}),
        (f"sbe $dateFromParts year {f}", {"$dateFromParts": {"year": f}}),
        (f"sbe $dateFromParts month {f}", {"$dateFromParts": {"year": 2020, "month": f}}),
        (f"sbe $dateFromParts tz {f}", {"$dateFromParts": {"year": 2020, "timezone": f}}),
        (f"sbe $dateTrunc {f}", {"$dateTrunc": {"date": f, "unit": "day"}}),
        (f"sbe $dateTrunc unit {f}", {"$dateTrunc": {"date": "$t", "unit": f}}),
        (
            f"sbe $dateTrunc bin {f}",
            {"$dateTrunc": {"date": "$t", "unit": "day", "binSize": f}},
        ),
        (f"sbe $dateFromString {f}", {"$dateFromString": {"dateString": f}}),
        (
            f"sbe $dateFromString onError {f}",
            {"$dateFromString": {"dateString": f, "onError": "E"}},
        ),
        (f"sbe $year tz {f}", {"$year": {"date": "$t", "timezone": f}}),
        (f"sbe $regexMatch regex {f}", {"$regexMatch": {"input": "$s", "regex": f}}),
        (
            f"sbe $regexMatch options {f}",
            {"$regexMatch": {"input": "$s", "regex": "$s", "options": f}},
        ),
        (f"sbe $cond {f}", {"$cond": [f, 1, 2]}),
        (f"sbe $switch {f}", {"$switch": {"branches": [{"case": f, "then": 1}], "default": 2}}),
        (f"sbe $let {f}", {"$let": {"vars": {"v": f}, "in": "$$v"}}),
        (
            f"sbe $dateAdd amount {f}",
            {"$dateAdd": {"startDate": "$t", "unit": "day", "amount": f}},
        ),
        (
            f"sbe $dateDiff start {f}",
            {"$dateDiff": {"startDate": f, "endDate": "$t2", "unit": "day"}},
        ),
    ]

# Values a builtin computes, beyond the type checks.
CASES += [
    ("sbe $setUnion order", {"$setUnion": [[3, 1, 2, 1], [5, 4, 3]]}, {"_id": 1}),
    (
        "sbe $setUnion order fields",
        {"$setUnion": ["$a", "$b"]},
        {"_id": 1, "a": [3, 1, 2, 1], "b": [5, 4, "x", 3]},
    ),
    (
        "sbe $setIntersection order fields",
        {"$setIntersection": ["$a", "$b"]},
        {"_id": 1, "a": [3, 1, 2, 5], "b": [5, 4, 3, 1]},
    ),
    (
        "sbe $setDifference order fields",
        {"$setDifference": ["$a", "$b"]},
        {"_id": 1, "a": [9, 3, 1, 2, 7, 1], "b": [5, 4, 3]},
    ),
    ("sbe $concat date", {"$concat": ["$s", "$t"]}),
    ("sbe $toUpper date", {"$toUpper": "$t"}),
    ("sbe $toLower ts", {"$toLower": "$ts"}),
    ("sbe $toUpper double", {"$toUpper": "$d"}),
    ("sbe $toUpper dec", {"$toUpper": "$dec"}),
    ("sbe $toUpper big", {"$toUpper": "$big"}),
    ("sbe $toUpper unicode", {"$toUpper": "$u"}),
    ("sbe $range big step", {"$range": [0, "$n", 1]}),
    ("sbe $range double", {"$range": [0, 5, "$d"]}),
    ("sbe $range double whole", {"$range": [0, 5, "$x"]}, {"_id": 1, "x": 2.0}),
    ("sbe $range decimal whole", {"$range": [0, 5, "$x"]}, {"_id": 1, "x": Decimal128("2")}),
    ("sbe $abs int min", {"$abs": "$x"}, {"_id": 1, "x": -2147483648}),
    ("sbe $abs long min", {"$abs": "$x"}, {"_id": 1, "x": Int64(-(2**63))}),
    ("sbe $pow int overflow", {"$pow": ["$x", 40]}, {"_id": 1, "x": 3}),
    ("sbe $pow neg exp", {"$pow": ["$x", -1]}, {"_id": 1, "x": 2}),
    ("sbe $multiply overflow", {"$multiply": ["$x", "$x"]}, {"_id": 1, "x": 3037000500}),
    ("sbe $add overflow", {"$add": ["$x", "$x"]}, {"_id": 1, "x": 2147483647}),
    ("sbe $mod neg", {"$mod": ["$x", 3]}, {"_id": 1, "x": -7}),
    ("sbe $mod double", {"$mod": ["$x", 2]}, {"_id": 1, "x": 7.5}),
    ("sbe $divide ints", {"$divide": ["$x", 4]}, {"_id": 1, "x": 10}),
    ("sbe $round long neg place", {"$round": ["$x", -2]}, {"_id": 1, "x": 1250}),
    ("sbe $trunc int neg place", {"$trunc": ["$x", -1]}, {"_id": 1, "x": 129}),
    ("sbe $round double place", {"$round": ["$x", 1]}, {"_id": 1, "x": 2.25}),
    ("sbe $substrCP unicode", {"$substrCP": ["$u", 1, 3]}),
    ("sbe $indexOfCP unicode", {"$indexOfCP": ["$u", "w"]}),
    ("sbe $indexOfBytes end", {"$indexOfBytes": ["$s", "c", 0, 2]}),
    ("sbe $strLenCP unicode", {"$strLenCP": "$u"}),
    ("sbe $objectToArray nested", {"$objectToArray": "$o"}),
    ("sbe $arrayToObject dup key", {"$arrayToObject": "$x"}, {"_id": 1, "x": [["a", 1], ["a", 2]]}),
    ("sbe $sortArray docs", {"$sortArray": {"input": "$docs", "sortBy": {"a": -1}}}),
    ("sbe $reverseArray", {"$reverseArray": "$arr"}),
    ("sbe $in missing arr", {"$in": [1, "$nope"]}),
    ("sbe $ifNull all null", {"$ifNull": ["$nul", "$nope"]}),
    ("sbe $and missing", {"$and": ["$nope", 1]}),
    ("sbe $cmp missing null", {"$cmp": ["$nope", "$nul"]}),
    ("sbe $eq missing null", {"$eq": ["$nope", "$nul"]}),
    ("sbe $dateToParts iso", {"$dateToParts": {"date": "$t", "iso8601": True}}),
    ("sbe $dateFromParts iso", {"$dateFromParts": {"isoWeekYear": "$n", "isoWeek": 60}}),
    (
        "sbe $dateFromParts big day",
        {"$dateFromParts": {"year": 2020, "day": "$x"}},
        {"_id": 1, "x": 40000},
    ),
    ("sbe $dateFromParts bad tz", {"$dateFromParts": {"year": 2020, "timezone": "$s"}}),
    (
        "sbe $dateTrunc week sow",
        {"$dateTrunc": {"date": "$t", "unit": "week", "startOfWeek": "$s"}},
    ),
    ("sbe $dateTrunc bin 2.5", {"$dateTrunc": {"date": "$t", "unit": "day", "binSize": "$d"}}),
    (
        "sbe $dateAdd long min",
        {"$dateSubtract": {"startDate": "$t", "unit": "day", "amount": "$x"}},
        {"_id": 1, "t": JAN1, "x": Int64(-(2**63))},
    ),
    ("sbe $dateFromString bad", {"$dateFromString": {"dateString": "$s"}}),
    ("sbe $dateFromString bad onError", {"$dateFromString": {"dateString": "$s", "onError": "E"}}),
    (
        "sbe $dateFromString format field",
        {"$dateFromString": {"dateString": "2020-01-02", "format": "$f"}},
        {"_id": 1, "f": "%Y-%m-%d"},
    ),
    (
        "sbe $dateFromString format bad field",
        {"$dateFromString": {"dateString": "2020-01-02", "format": "$f"}},
        {"_id": 1, "f": "%Q"},
    ),
    (
        "sbe $dateToString format field",
        {"$dateToString": {"date": "$t", "format": "$f"}},
        {"_id": 1, "t": JAN1, "f": "%Y"},
    ),
    (
        "sbe $dateToString format bad field",
        {"$dateToString": {"date": "$t", "format": "$f"}},
        {"_id": 1, "t": JAN1, "f": "%Q"},
    ),
    (
        "sbe $dateToString tz bad field",
        {"$dateToString": {"date": "$t", "timezone": "$f"}},
        {"_id": 1, "t": JAN1, "f": "Mars/Base"},
    ),
    ("sbe $year tz bad field", {"$year": {"date": "$t", "timezone": "$s"}}),
    ("sbe $year tz null const", {"$year": {"date": "$t", "timezone": None}}),
    ("sbe $year tz bad const", {"$year": {"date": "$t", "timezone": "Mars/Base"}}),
    (
        "sbe $regexFind field pattern",
        {"$regexFind": {"input": "$s", "regex": "$x"}},
        {"_id": 1, "s": "abc", "x": "b"},
    ),
    (
        "sbe $regexFind field bad pattern",
        {"$regexFind": {"input": "$s", "regex": "$x"}},
        {"_id": 1, "s": "abc", "x": "("},
    ),
    (
        "sbe $regexFindAll field pattern",
        {"$regexFindAll": {"input": "$s", "regex": "$x"}},
        {"_id": 1, "s": "abcb", "x": "b"},
    ),
    (
        "sbe $regexMatch regex obj options field",
        {"$regexMatch": {"input": "$s", "regex": "$re", "options": "$x"}},
        {"_id": 1, "s": "abc", "re": Regex("B", ""), "x": "i"},
    ),
    (
        "sbe $regexMatch pattern int options",
        {"$regexMatch": {"input": "$s", "regex": "$x", "options": "i"}},
        {"_id": 1, "s": "abc", "x": 5},
    ),
]
