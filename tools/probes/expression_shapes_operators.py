"""Expression shapes for `expression_shapes.py`: arrays, strings, objects, conversion,
regex and equality (2026-10-21).

Written against the uncovered lines of `secantus-core/src/expressions.rs` behind
`$switch`, `$mod`, `$arrayElemAt`, the `$firstN` family, `$sortArray`,
`$arrayToObject`, `$substrCP` / `$substrBytes`, `$zip`, `$split`, `$slice`,
`$getField` / `$setField`, `$filter`, `$cmp`, `$strcasecmp`, `$median` /
`$percentile`, `$meta`, `$convert` and the `$to*` family, the regex operators,
equality in `$eq` / `$in` / `$indexOfArray`, and the `$indexOf*` positions.
Labels ending `[const]` / `[field]` are the folded and per-document forms of the
same shape, which mongod can answer differently.
"""

import datetime
import uuid

from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp
from bson.datetime_ms import DatetimeMS

D = Decimal128
L = Int64
NAN = float("nan")
INF = float("inf")
CASES = []


def both(label, op, args, extra=None):
    """constant form and field-path form: args list; field form puts values in doc as x0,x1.."""
    CASES.append(
        (
            label + " [const]",
            {op: [{"$literal": a} if isinstance(a, (list, str)) else a for a in args]},
        )
    )
    doc = {"_id": 1}
    fargs = []
    for i, a in enumerate(args):
        doc[f"x{i}"] = a
        fargs.append(f"$x{i}")
    CASES.append((label + " [field]", {op: fargs}, doc))


# ---- $switch
sw = "$switch"
CASES += [
    (
        "switch match first",
        {sw: {"branches": [{"case": {"$gt": ["$n", 1]}, "then": "big"}], "default": "small"}},
    ),
    (
        "switch default",
        {sw: {"branches": [{"case": {"$gt": ["$n", 5]}, "then": "big"}], "default": "small"}},
    ),
    (
        "switch no match no default field",
        {sw: {"branches": [{"case": {"$gt": ["$n", 5]}, "then": 1}]}},
    ),
    ("switch no match no default const", {sw: {"branches": [{"case": False, "then": 1}]}}),
    ("switch branches not array", {sw: {"branches": 1}}),
    ("switch branches missing", {sw: {"default": 1}}),
    ("switch branches empty", {sw: {"branches": [], "default": 3}}),
    ("switch branches empty no default", {sw: {"branches": []}}),
    ("switch branch not doc", {sw: {"branches": [1], "default": 3}}),
    ("switch branch missing then", {sw: {"branches": [{"case": True}], "default": 3}}),
    ("switch branch missing case", {sw: {"branches": [{"then": 1}], "default": 3}}),
    ("switch branch extra key", {sw: {"branches": [{"case": True, "then": 1, "x": 2}]}}),
    ("switch unknown arg", {sw: {"branches": [{"case": True, "then": 1}], "foo": 1}}),
    ("switch not doc", {sw: [1]}),
    ("switch not doc str", {sw: "x"}),
    (
        "switch case truthy 0",
        {
            sw: {
                "branches": [
                    {"case": "$nul", "then": 1},
                    {"case": 0, "then": 2},
                    {"case": "$s", "then": 3},
                ]
            }
        },
    ),
    (
        "switch case missing field",
        {sw: {"branches": [{"case": "$nope", "then": 1}], "default": "$nope"}},
    ),
    ("switch then missing", {sw: {"branches": [{"case": True, "then": "$nope"}]}}),
    (
        "switch default expr",
        {sw: {"branches": [{"case": "$nul", "then": 1}], "default": {"$add": ["$n", 1]}}},
    ),
    ("switch then array", {sw: {"branches": [{"case": "$arr", "then": "$arr"}]}}),
    ("switch case empty array", {sw: {"branches": [{"case": [], "then": 1}], "default": 2}}),
    (
        "switch case error lazy",
        {sw: {"branches": [{"case": True, "then": 1}, {"case": {"$divide": [1, 0]}, "then": 2}]}},
    ),
    ("switch then error later", {sw: {"branches": [{"case": "$n", "then": {"$divide": [1, 0]}}]}}),
    (
        "switch default not evaluated",
        {sw: {"branches": [{"case": "$n", "then": 1}], "default": {"$divide": ["$n", 0]}}},
    ),
    ("switch branches field path", {sw: {"branches": "$arr"}}),
]

# ---- $mod
for lab, args in [
    ("mod int by zero", [5, 0]),
    ("mod double by zero", [5.5, 0]),
    ("mod by 0.0", [5, 0.0]),
    ("mod long by zero", [L(5), L(0)]),
    ("mod null", [None, 2]),
    ("mod null2", [5, None]),
    ("mod dec by zero", [D("5"), 0]),
    ("mod nan", [NAN, 2]),
    ("mod inf", [INF, 2]),
    ("mod by inf", [5, INF]),
    ("mod neg", [-7, 3]),
    ("mod double", [7.5, 2]),
    ("mod long int", [L(7), 2]),
    ("mod int long", [7, L(2)]),
    ("mod dec", [D("7.5"), 2]),
    ("mod int dec", [7, D("2.5")]),
    ("mod double long", [7.5, L(2)]),
    ("mod int min -1", [-2147483648, -1]),
    ("mod long min -1", [L(-(2**63)), -1]),
    ("mod string", ["a", 2]),
    ("mod date", [datetime.datetime(2020, 1, 1), 2]),
    ("mod dec nan", [D("NaN"), 2]),
    ("mod dec inf", [D("Infinity"), 2]),
    ("mod by dec 0", [5, D("0")]),
    ("mod by neg zero", [5, -0.0]),
    ("mod dec by dec neg", [D("-7"), D("3")]),
]:
    both(lab, "$mod", args)
CASES += [
    ("mod one arg", {"$mod": [5]}),
    ("mod three args", {"$mod": [5, 2, 1]}),
    ("mod missing", {"$mod": ["$nope", 2]}),
    ("mod missing2", {"$mod": [5, "$nope"]}),
]

# ---- $arrayElemAt
ae = "$arrayElemAt"
for lab, args in [
    ("ae dec idx", [[1, 2, 3], D("1")]),
    ("ae dec idx frac", [[1, 2, 3], D("1.5")]),
    ("ae dec idx huge", [[1, 2, 3], D("1E+40")]),
    ("ae dec idx nan", [[1, 2, 3], D("NaN")]),
    ("ae dec idx inf", [[1, 2, 3], D("Infinity")]),
    ("ae dec idx neg", [[1, 2, 3], D("-1")]),
    ("ae dec idx 1.0", [[1, 2, 3], D("1.0")]),
    ("ae dec idx 2^31", [[1, 2, 3], D("2147483648")]),
    ("ae long big", [[1, 2, 3], L(2**31)]),
    ("ae long ok", [[1, 2, 3], L(1)]),
    ("ae double frac", [[1, 2, 3], 1.5]),
    ("ae double 1e40", [[1, 2, 3], 1e40]),
    ("ae double nan", [[1, 2, 3], NAN]),
    ("ae double inf", [[1, 2, 3], INF]),
    ("ae double 2.0", [[1, 2, 3], 2.0]),
    ("ae double -0.0", [[1, 2, 3], -0.0]),
    ("ae str idx", [[1, 2, 3], "x"]),
    ("ae null idx", [[1, 2, 3], None]),
    ("ae bool idx", [[1, 2, 3], True]),
    ("ae date idx", [[1, 2, 3], datetime.datetime(2020, 1, 1)]),
    ("ae oid idx", [[1, 2, 3], ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")]),
    ("ae arr idx", [[1, 2, 3], [1]]),
    ("ae obj idx", [[1, 2, 3], {"a": 1}]),
    ("ae null arr", [None, 1]),
    ("ae null arr bad idx", [None, "x"]),
    ("ae null arr frac idx", [None, 1.5]),
    ("ae str arr", ["abc", 1]),
    ("ae out of range", [[1, 2], 9]),
    ("ae neg out of range", [[1, 2], -9]),
    ("ae neg", [[1, 2], -1]),
    ("ae empty", [[], 0]),
    ("ae int min", [[1, 2], -2147483648]),
    ("ae ts idx", [[1], Timestamp(1, 1)]),
    ("ae bin idx", [[1], Binary(b"x", 0)]),
    ("ae regex idx", [[1], Regex("a")]),
    ("ae minkey idx", [[1], MinKey()]),
]:
    both(lab, ae, args)
CASES += [
    ("ae missing arr", {ae: ["$nope", 1]}),
    ("ae missing idx", {ae: ["$arr", "$nope"]}),
    ("ae missing both", {ae: ["$nope", "$nope2"]}),
    ("ae one arg", {ae: ["$arr"]}),
    ("ae not array", {ae: "$arr"}),
    ("ae three", {ae: ["$arr", 1, 2]}),
    ("ae missing arr bad idx", {ae: ["$nope", "x"]}),
]

# ---- nelem via $firstN/$lastN/$maxN/$minN
for op in ["$firstN", "$lastN", "$maxN", "$minN"]:
    for lab, n in [
        ("bool", True),
        ("long", L(2)),
        ("long0", L(0)),
        ("dec", D("2")),
        ("dec frac", D("2.5")),
        ("dec nan", D("NaN")),
        ("str", "x"),
        ("null", None),
        ("nan", NAN),
        ("inf", INF),
        ("neg", -1),
        ("huge long", L(2**62)),
        ("double2", 2.0),
        ("double frac", 1.5),
        ("date", datetime.datetime(2020, 1, 1)),
        ("arr", [1]),
        ("obj", {"a": 1}),
        ("missing", "$nope"),
        ("neg double", -2.0),
        ("dec neg", D("-1")),
    ]:
        CASES.append((f"{op} n {lab}", {op: {"n": n, "input": "$arr"}}))
    CASES += [
        (f"{op} no n", {op: {"input": "$arr"}}),
        (f"{op} no input", {op: {"n": 1}}),
        (f"{op} input null", {op: {"n": 1, "input": None}}),
        (f"{op} input missing", {op: {"n": 1, "input": "$nope"}}),
        (f"{op} not doc", {op: [1, 2]}),
        (f"{op} extra key", {op: {"n": 1, "input": "$arr", "x": 1}}),
        (f"{op} n field", {op: {"n": "$n", "input": "$arr"}}),
        (f"{op} n field big", {op: {"n": "$big", "input": "$arr"}}),
        (f"{op} n field dec", {op: {"n": "$dec", "input": "$arr"}}),
        (f"{op} input mixed", {op: {"n": 2, "input": [None, 3, "a", 1, None]}}),
        (f"{op} n str field", {op: {"n": "$s", "input": "$arr"}}),
    ]


D = Decimal128
L = Int64
NAN = float("nan")
INF = float("inf")
DT = datetime.datetime(2020, 1, 1)


def lit(a):
    return {"$literal": a} if isinstance(a, (list, str, dict)) else a


def both(label, op, args):
    CASES.append((label + " [const]", {op: [lit(a) for a in args]}))
    doc = {"_id": 1}
    fargs = []
    for i, a in enumerate(args):
        doc[f"x{i}"] = a
        fargs.append(f"$x{i}")
    CASES.append((label + " [field]", {op: fargs}, doc))


def bothdoc(label, op, spec, fieldkeys):
    """spec dict; constant form and form where keys in fieldkeys are moved to doc fields"""
    CASES.append(
        (label + " [const]", {op: {k: (lit(v) if k in fieldkeys else v) for k, v in spec.items()}})
    )
    doc = {"_id": 1}
    s2 = {}
    for k, v in spec.items():
        if k in fieldkeys:
            doc["f_" + k] = v
            s2[k] = "$f_" + k
        else:
            s2[k] = v
    CASES.append((label + " [field]", {op: s2}, doc))


# ---- $sortArray
sa = "$sortArray"
for lab, inp, by in [
    ("asc ints", [3, 1, 2], 1),
    ("desc ints", [3, 1, 2], -1),
    ("long asc", [3, 1, 2], L(1)),
    ("long desc", [3, 1, 2], L(-1)),
    ("double asc", [3, 1, 2], 1.0),
    ("by 0", [3, 1, 2], 0),
    ("by 2", [3, 1, 2], 2),
    ("by str", [3, 1, 2], "a"),
    ("by bool", [3, 1, 2], True),
    ("by dec 1", [3, 1, 2], D("1")),
    ("by -1.0", [3, 1, 2], -1.0),
    (
        "mixed types",
        [3, "a", None, [1], {"a": 1}, True, 1.5, D("2"), DT, ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")],
        1,
    ),
    ("mixed desc", [3, "a", None, [1], {"a": 1}, True, 1.5, D("2"), DT], -1),
    ("arrays", [[2, 1], [1], [], [3], [0, 9]], 1),
    ("arrays desc", [[2, 1], [1], [], [3], [0, 9]], -1),
    ("docs by a", [{"a": 2}, {"a": 1}, {"a": 3}, {"b": 1}], {"a": 1}),
    ("docs by a desc", [{"a": 2}, {"a": 1}, {"a": 3}, {"b": 1}], {"a": -1}),
    ("docs by a b", [{"a": 1, "b": 2}, {"a": 1, "b": 1}, {"a": 0, "b": 3}], {"a": 1, "b": -1}),
    ("docs dotted", [{"a": {"b": 2}}, {"a": {"b": 1}}, {"a": [{"b": 0}]}], {"a.b": 1}),
    ("docs array field", [{"a": [3, 1]}, {"a": [2]}, {"a": 0}], {"a": 1}),
    ("docs array field desc", [{"a": [3, 1]}, {"a": [2]}, {"a": 0}], {"a": -1}),
    ("scalars by doc", [3, 1, {"a": 1}, {"a": 0}], {"a": 1}),
    ("by doc 0", [{"a": 2}, {"a": 1}], {"a": 0}),
    ("by doc str", [{"a": 2}, {"a": 1}], {"a": "x"}),
    ("by empty doc", [{"a": 2}, {"a": 1}], {}),
    ("by doc 2", [{"a": 2}, {"a": 1}], {"a": 2}),
    ("by doc textScore", [{"a": 2}, {"a": 1}], {"a": {"$meta": "textScore"}}),
    ("by doc double", [{"a": 2}, {"a": 1}], {"a": -1.0}),
    ("stable", [{"a": 1, "i": 0}, {"a": 0, "i": 1}, {"a": 1, "i": 2}, {"a": 0, "i": 3}], {"a": 1}),
    ("numbers mixed", [2, L(1), 1.5, D("0.5"), NAN, -INF], 1),
    ("nan desc", [NAN, 1, D("NaN")], -1),
    ("empty", [], 1),
    ("null elem", [None, 1], 1),
    ("strings", ["b", "B", "a", "é", ""], 1),
    ("minmax", [MaxKey(), 1, MinKey()], 1),
    ("binary", [Binary(b"\x02", 0), Binary(b"\x01\x01", 0), Binary(b"\x01", 5)], 1),
    ("regex", [Regex("b"), Regex("a", "i"), Regex("a")], 1),
    ("ts", [Timestamp(2, 1), Timestamp(1, 5)], 1),
    ("code", [Code("b"), Code("a")], 1),
]:
    bothdoc(f"sa {lab}", sa, {"input": inp, "sortBy": by}, {"input"})
CASES += [
    ("sa input null", {sa: {"input": None, "sortBy": 1}}),
    ("sa input missing", {sa: {"input": "$nope", "sortBy": 1}}),
    ("sa input str", {sa: {"input": "$s", "sortBy": 1}}),
    ("sa input int const", {sa: {"input": 5, "sortBy": 1}}),
    ("sa no sortBy", {sa: {"input": "$arr"}}),
    ("sa no input", {sa: {"sortBy": 1}}),
    ("sa extra", {sa: {"input": "$arr", "sortBy": 1, "x": 1}}),
    ("sa not doc", {sa: "$arr"}),
    ("sa arr arg", {sa: ["$arr", 1]}),
    ("sa sortBy expr", {sa: {"input": "$arr", "sortBy": "$n"}}),
    ("sa input obj", {sa: {"input": "$o", "sortBy": 1}}),
    ("sa docs field", {sa: {"input": "$docs", "sortBy": {"a": -1}}}),
    ("sa docs field b", {sa: {"input": "$docs", "sortBy": {"b": 1}}}),
]

# ---- $arrayToObject
ato = "$arrayToObject"
for lab, v in [
    ("kv ok", [{"k": "a", "v": 1}, {"k": "b", "v": 2}]),
    ("kv reversed keys", [{"v": 1, "k": "a"}]),
    ("kv dup", [{"k": "a", "v": 1}, {"k": "a", "v": 2}]),
    ("kv missing v", [{"k": "a", "x": 1}]),
    ("kv 3 keys", [{"k": "a", "v": 1, "x": 2}]),
    ("kv 1 key", [{"k": "a"}]),
    ("kv k int", [{"k": 1, "v": 1}]),
    ("kv k null", [{"k": None, "v": 1}]),
    ("kv k nul byte", [{"k": "a\x00b", "v": 1}]),
    ("kv then array", [{"k": "a", "v": 1}, ["b", 2]]),
    ("kv then int", [{"k": "a", "v": 1}, 5]),
    ("pairs ok", [["a", 1], ["b", 2]]),
    ("pairs dup", [["a", 1], ["a", 2]]),
    ("pairs 3", [["a", 1, 2]]),
    ("pairs 1", [["a"]]),
    ("pairs 0", [[]]),
    ("pairs k int", [[1, 2]]),
    ("pairs k null", [[None, 2]]),
    ("pairs nul byte", [["a\x00", 1]]),
    ("pairs then doc", [["a", 1], {"k": "b", "v": 2}]),
    ("pairs then str", [["a", 1], "x"]),
    ("first str", ["x"]),
    ("first int", [1]),
    ("first null", [None]),
    ("empty", []),
    ("dollar key", [["$a", 1]]),
    ("dotted key", [["a.b", 1]]),
    ("empty key", [["", 1]]),
    ("kv v missing field?", [{"k": "a", "v": None}]),
    ("kv k dollar", [{"k": "$x", "v": 1}]),
    ("pairs second elem bad", [["a", 1], ["b"]]),
    ("kv second bad", [{"k": "a", "v": 1}, {"k": "b"}]),
    ("nested", [["a", [["b", 1]]]]),
    ("unicode key", [["é", 1]]),
]:
    CASES.append((f"ato {lab} [const]", {ato: {"$literal": v}}))
    CASES.append((f"ato {lab} [field]", {ato: "$x"}, {"_id": 1, "x": v}))
CASES += [
    ("ato wrapped", {ato: [[["a", 1]]]}),
    ("ato wrapped field", {ato: ["$docs"]}),
    ("ato missing", {ato: "$nope"}),
    ("ato two args", {ato: [[["a", 1]], [["b", 2]]]}),
    ("ato obj", {ato: "$o"}),
    ("ato literal arr expr", {ato: [[{"k": "a", "v": "$n"}]]}),
]

# ---- $substrCP
for lab, args in [
    ("bool start", ["abc", True, 1]),
    ("bool len", ["abc", 0, False]),
    ("neg start", ["abc", -1, 1]),
    ("neg len", ["abc", 0, -1]),
    ("null str", [None, 0, 1]),
    ("int str", [12345, 1, 2]),
    ("double str", [1.5, 0, 2]),
    ("dec str", [D("1.50"), 0, 3]),
    ("date str", [DT, 0, 4]),
    ("bool str", [True, 0, 1]),
    ("oid str", [ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), 0, 1]),
    ("arr str", [[1], 0, 1]),
    ("obj str", [{"a": 1}, 0, 1]),
    ("ts str", [Timestamp(1, 1), 0, 3]),
    ("bin str", [Binary(b"a", 0), 0, 1]),
    ("long str", [L(99), 0, 1]),
    ("minkey str", [MinKey(), 0, 1]),
    ("regex str", [Regex("a"), 0, 1]),
    ("unicode", ["héllo wörld", 1, 4]),
    ("emoji", ["a😀b", 1, 1]),
    ("emoji 2", ["a😀b", 2, 5]),
    ("start past end", ["abc", 5, 1]),
    ("len past end", ["abc", 1, 100]),
    ("zero len", ["abc", 1, 0]),
    ("double start", ["abc", 1.0, 1.0]),
    ("frac start", ["abc", 1.5, 1]),
    ("frac len", ["abc", 1, 1.5]),
    ("dec start", ["abc", D("1"), D("1")]),
    ("dec frac", ["abc", D("1.5"), 1]),
    ("long start", ["abc", L(1), L(1)]),
    ("long huge start", ["abc", L(2**40), 1]),
    ("long huge len", ["abc", 0, L(2**40)]),
    ("nan start", ["abc", NAN, 1]),
    ("inf len", ["abc", 0, INF]),
    ("str start", ["abc", "1", 1]),
    ("str len", ["abc", 1, "x"]),
    ("null start", ["abc", None, 1]),
    ("null len", ["abc", 0, None]),
    ("dec nan len", ["abc", 0, D("NaN")]),
    ("dec huge len", ["abc", 0, D("3E+9")]),
    ("int max len", ["abc", 1, 2147483647]),
    ("int max start", ["abc", 2147483647, 1]),
    ("neg zero start", ["abc", -0.0, 2]),
    ("empty str", ["", 0, 1]),
    ("double huge start", ["abc", 3e9, 1]),
]:
    both(f"scp {lab}", "$substrCP", args)
CASES += [
    ("scp missing str", {"$substrCP": ["$nope", 0, 1]}),
    ("scp two args", {"$substrCP": ["abc", 0]}),
    ("scp missing start", {"$substrCP": ["abc", "$nope", 1]}),
    ("scp missing len", {"$substrCP": ["abc", 0, "$nope"]}),
]

# ---- $substrBytes / $substr
for op in ["$substrBytes", "$substr"]:
    for lab, args in [
        ("basic", ["abcdef", 1, 3]),
        ("neg len", ["abcdef", 1, -1]),
        ("neg start", ["abcdef", -1, 2]),
        ("null str", [None, 0, 1]),
        ("int str", [12345, 1, 2]),
        ("double str", [2.5, 0, 2]),
        ("date str", [DT, 0, 4]),
        ("arr str", [[1], 0, 1]),
        ("obj str", [{"a": 1}, 0, 1]),
        ("bool str", [True, 0, 1]),
        ("oid str", [ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), 0, 2]),
        ("mid utf8 start", ["héllo", 2, 2]),
        ("mid utf8 end", ["héllo", 0, 2]),
        ("utf8 ok", ["héllo", 1, 2]),
        ("start past end", ["abc", 10, 2]),
        ("len past end", ["abc", 1, 100]),
        ("double idx", ["abcdef", 1.7, 2.9]),
        ("neg double len", ["abcdef", 1, -0.5]),
        ("dec idx", ["abcdef", D("1.5"), D("2.5")]),
        ("dec idx even", ["abcdef", D("2.5"), D("1")]),
        ("long idx", ["abcdef", L(1), L(2)]),
        ("nan idx", ["abcdef", NAN, 1]),
        ("inf len", ["abcdef", 0, INF]),
        ("huge double", ["abcdef", 1e19, 1]),
        ("dec nan", ["abcdef", D("NaN"), 1]),
        ("dec huge", ["abcdef", D("1E+20"), 1]),
        ("str idx", ["abc", "1", 1]),
        ("bool idx", ["abc", True, 1]),
        ("null idx", ["abc", None, 1]),
        ("null len", ["abc", 0, None]),
        ("emoji mid", ["a😀b", 2, 1]),
        ("emoji end mid", ["a😀b", 0, 3]),
        ("emoji whole", ["a😀b", 1, 4]),
        ("long huge len", ["abc", 0, L(2**62)]),
        ("neg long len", ["abc", 1, L(-5)]),
        ("start at end", ["abc", 3, 1]),
        ("dec neg len", ["abcdef", 1, D("-1")]),
        ("ts str", [Timestamp(5, 1), 0, 20]),
    ]:
        both(f"{op} {lab}", op, args)


D = Decimal128
L = Int64
NAN = float("nan")
INF = float("inf")
DT = datetime.datetime(2020, 1, 1)


def both(label, op, args):
    CASES.append((label + " [const]", {op: [lit(a) for a in args]}))
    doc = {"_id": 1}
    fargs = []
    for i, a in enumerate(args):
        doc[f"x{i}"] = a
        fargs.append(f"$x{i}")
    CASES.append((label + " [field]", {op: fargs}, doc))


def bothdoc(label, op, spec, fieldkeys):
    CASES.append(
        (label + " [const]", {op: {k: (lit(v) if k in fieldkeys else v) for k, v in spec.items()}})
    )
    doc = {"_id": 1}
    s2 = {}
    for k, v in spec.items():
        if k in fieldkeys:
            doc["f_" + k] = v
            s2[k] = "$f_" + k
        else:
            s2[k] = v
    CASES.append((label + " [field]", {op: s2}, doc))


# substrBytes extra decimal/double edges
for op in ["$substrBytes"]:
    for lab, args in [
        ("dec huge len", ["abcdef", 1, D("1E+20")]),
        ("dec nan len", ["abcdef", 1, D("NaN")]),
        ("dec inf start", ["abcdef", D("Infinity"), 1]),
        ("dec -inf len", ["abcdef", 1, D("-Infinity")]),
        ("dbl huge len", ["abcdef", 1, 1e20]),
        ("dbl nan len", ["abcdef", 1, NAN]),
        ("dbl -inf start", ["abcdef", -INF, 1]),
        ("dec neg huge start", ["abcdef", D("-1E+20"), 1]),
        ("dec 2.5 len", ["abcdef", 0, D("2.5")]),
        ("dec 3.5 len", ["abcdef", 0, D("3.5")]),
    ]:
        both(f"{op} {lab}", op, args)

# ---- $zip
z = "$zip"
CASES += [
    ("zip basic", {z: {"inputs": [[1, 2, 3], ["a", "b"]]}}),
    ("zip field", {z: {"inputs": ["$arr", "$docs"]}}),
    ("zip longest", {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": True}}),
    (
        "zip longest defaults",
        {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": True, "defaults": [0, "z"]}},
    ),
    ("zip defaults no longest", {z: {"inputs": [[1, 2, 3], ["a"]], "defaults": [0, "z"]}}),
    (
        "zip defaults wrong len",
        {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": True, "defaults": [0]}},
    ),
    (
        "zip defaults not array",
        {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": True, "defaults": 5}},
    ),
    (
        "zip defaults empty",
        {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": True, "defaults": []}},
    ),
    (
        "zip defaults null",
        {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": True, "defaults": None}},
    ),
    (
        "zip defaults field",
        {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": True, "defaults": "$arr"}},
    ),
    (
        "zip defaults exprs",
        {
            z: {
                "inputs": [[1, 2, 3], ["a"]],
                "useLongestLength": True,
                "defaults": ["$n", {"$add": [1, 1]}],
            }
        },
    ),
    ("zip longest 1", {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": 1}}),
    ("zip longest str", {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": "yes"}}),
    ("zip longest false", {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": False}}),
    ("zip longest null", {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": None}}),
    ("zip longest field", {z: {"inputs": [[1, 2, 3], ["a"]], "useLongestLength": "$n"}}),
    ("zip inputs empty", {z: {"inputs": []}}),
    ("zip inputs not array", {z: {"inputs": 5}}),
    ("zip inputs field", {z: {"inputs": "$arr"}}),
    ("zip no inputs", {z: {"useLongestLength": True}}),
    ("zip unknown", {z: {"inputs": [[1]], "foo": 1}}),
    ("zip not doc", {z: [[1], [2]]}),
    ("zip null input", {z: {"inputs": [[1, 2], None]}}),
    ("zip missing input", {z: {"inputs": [[1, 2], "$nope"]}}),
    ("zip null input field", {z: {"inputs": ["$arr", "$nul"]}}),
    ("zip scalar input field", {z: {"inputs": ["$arr", "$n"]}}),
    ("zip scalar then null", {z: {"inputs": ["$n", "$nul"]}}),
    ("zip null then scalar", {z: {"inputs": ["$nul", "$n"]}}),
    ("zip empty arrays", {z: {"inputs": [[], [1]]}}),
    ("zip empty arrays longest", {z: {"inputs": [[], [1]], "useLongestLength": True}}),
    ("zip single", {z: {"inputs": ["$arr"]}}),
    (
        "zip input expr",
        {
            z: {
                "inputs": [{"$concatArrays": ["$arr", [9]]}, "$arr"],
                "useLongestLength": True,
                "defaults": [0, -1],
            }
        },
    ),
    ("zip input literal str", {z: {"inputs": [[1], "abc"]}}),
    ("zip input const scalar", {z: {"inputs": [[1], 5]}}),
    ("zip defaults no longest field", {z: {"inputs": ["$arr"], "defaults": [1]}}),
    ("zip input obj field", {z: {"inputs": ["$o"]}}),
]

# ---- $split
for lab, args in [
    ("empty sep", ["abc", ""]),
    ("null str", [None, "a"]),
    ("null sep", ["abc", None]),
    ("both null", [None, None]),
    ("unicode sep", ["héllo wörld", "ö"]),
    ("sep at ends", ["-a-b-", "-"]),
    ("sep longer", ["ab", "abc"]),
    ("empty str", ["", "a"]),
    ("int str", [5, "a"]),
    ("int sep", ["abc", 5]),
    ("regex sep", ["abc", Regex("b")]),
    ("null str bad sep", [None, 5]),
    ("bad str null sep", [5, None]),
    ("nul char sep", ["a\x00b", "\x00"]),
    ("multi", ["a,,b", ","]),
    ("same", ["abc", "abc"]),
]:
    both(f"split {lab}", "$split", args)
CASES += [
    ("split missing", {"$split": ["$nope", "a"]}),
    ("split missing sep", {"$split": ["abc", "$nope"]}),
    ("split one arg", {"$split": ["abc"]}),
    ("split three", {"$split": ["abc", "b", "c"]}),
    ("split not array", {"$split": "abc"}),
]

# ---- $slice
for lab, args in [
    ("bool n", [[1, 2, 3], True]),
    ("bool pos", [[1, 2, 3], False, 1]),
    ("bool n3", [[1, 2, 3], 0, True]),
    ("null arr", [None, 1]),
    ("null arr bad n", [None, "x"]),
    ("null n", [[1, 2, 3], None]),
    ("null pos", [[1, 2, 3], None, 1]),
    ("null n3", [[1, 2, 3], 0, None]),
    ("neg pos", [[1, 2, 3], -2, 1]),
    ("neg pos big", [[1, 2, 3], -10, 2]),
    ("pos past", [[1, 2, 3], 10, 2]),
    ("n zero", [[1, 2, 3], 0]),
    ("n neg big", [[1, 2, 3], -10]),
    ("n huge", [[1, 2, 3], 2147483647]),
    ("n int min", [[1, 2, 3], -2147483648]),
    ("pos frac", [[1, 2, 3], 1.5, 1]),
    ("pos long big", [[1, 2, 3], L(2**31), 1]),
    ("pos dec", [[1, 2, 3], D("1"), D("1")]),
    ("n dec frac", [[1, 2, 3], D("1.5")]),
    ("n3 str", [[1, 2, 3], 0, "a"]),
    ("pos str", [[1, 2, 3], "a", 1]),
    ("n3 nan", [[1, 2, 3], 0, NAN]),
    ("n3 dec nan", [[1, 2, 3], 0, D("NaN")]),
    ("n3 neg", [[1, 2, 3], 0, -1]),
    ("n3 zero dbl", [[1, 2, 3], 0, 0.0]),
    ("empty arr", [[], 1]),
    ("empty arr 3", [[], -1, 1]),
    ("n3 big", [[1, 2, 3], 1, 2147483647]),
    ("pos int min", [[1, 2, 3], -2147483648, 1]),
]:
    both(f"slice {lab}", "$slice", args)
CASES += [
    ("slice missing arr", {"$slice": ["$nope", 1]}),
    ("slice one arg", {"$slice": ["$arr"]}),
    ("slice four", {"$slice": ["$arr", 1, 1, 1]}),
    ("slice missing n", {"$slice": ["$arr", "$nope"]}),
    ("slice not array", {"$slice": "$arr"}),
]

# ---- $getField / $setField / $unsetField
gf = "$getField"
CASES += [
    ("gf no field", {gf: {"input": "$o"}}),
    ("gf no input", {gf: {"field": "k"}}),
    ("gf input missing path", {gf: {"field": "k", "input": "$nope"}}),
    ("gf input path", {gf: {"field": "k", "input": "$o"}}),
    ("gf input null", {gf: {"field": "k", "input": None}}),
    ("gf input null path", {gf: {"field": "k", "input": "$nul"}}),
    ("gf input scalar", {gf: {"field": "k", "input": 5}}),
    ("gf input scalar path", {gf: {"field": "k", "input": "$n"}}),
    ("gf input array path", {gf: {"field": "a", "input": "$docs"}}),
    ("gf input $$CURRENT", {gf: {"field": "n", "input": "$$CURRENT"}}),
    ("gf input $$ROOT", {gf: {"field": "s", "input": "$$ROOT"}}),
    ("gf field expr", {gf: {"field": {"$concat": ["k", ""]}, "input": "$o"}}),
    ("gf field int", {gf: {"field": 5, "input": "$o"}}),
    ("gf field path", {gf: {"field": "$s", "input": "$o"}}),
    ("gf field null", {gf: {"field": None, "input": "$o"}}),
    ("gf unknown arg", {gf: {"field": "k", "input": "$o", "x": 1}}),
    ("gf dotted", {gf: {"field": "j.x", "input": "$o"}}),
    ("gf dotted literal", {gf: "a.b"}, {"_id": 1, "a.b": 7, "a": {"b": 8}}),
    ("gf dollar literal", {gf: {"$literal": "$x"}}, {"_id": 1, "$x": 3}),
    (
        "gf dollar field opt",
        {gf: {"field": {"$literal": "$x"}, "input": "$$ROOT"}},
        {"_id": 1, "$x": 3},
    ),
    ("gf input literal doc", {gf: {"field": "a", "input": {"a": "$n"}}}),
    ("gf input literal $literal", {gf: {"field": "a", "input": {"$literal": {"a": "$n"}}}}),
    ("gf string shorthand", {gf: "n"}),
    ("gf string missing", {gf: "nope"}),
    ("gf field expr shorthand", {gf: {"$concat": ["n"]}}),
    ("gf input missing const doc", {gf: {"field": "zz", "input": {"a": 1}}}),
    ("gf array arg", {gf: ["n"]}),
    ("gf input path nested", {gf: {"field": "x", "input": "$o.j"}}),
    ("gf input $$REMOVE", {gf: {"field": "x", "input": "$$REMOVE"}}),
]
sf = "$setField"
CASES += [
    ("sf basic", {sf: {"field": "k", "input": "$o", "value": 9}}),
    ("sf new", {sf: {"field": "new", "input": "$o", "value": "$n"}}),
    ("sf remove", {sf: {"field": "k", "input": "$o", "value": "$$REMOVE"}}),
    ("sf missing value path", {sf: {"field": "k", "input": "$o", "value": "$nope"}}),
    ("sf input null", {sf: {"field": "k", "input": None, "value": 1}}),
    ("sf input missing", {sf: {"field": "k", "input": "$nope", "value": 1}}),
    ("sf input scalar", {sf: {"field": "k", "input": "$n", "value": 1}}),
    ("sf no value", {sf: {"field": "k", "input": "$o"}}),
    ("sf no input", {sf: {"field": "k", "value": 1}}),
    ("sf no field", {sf: {"input": "$o", "value": 1}}),
    ("sf field int", {sf: {"field": 5, "input": "$o", "value": 1}}),
    ("sf field path", {sf: {"field": "$s", "input": "$o", "value": 1}}),
    ("sf field literal dollar", {sf: {"field": {"$literal": "$k"}, "input": "$o", "value": 1}}),
    ("sf field literal int", {sf: {"field": {"$literal": 5}, "input": "$o", "value": 1}}),
    ("sf field expr", {sf: {"field": {"$concat": ["a"]}, "input": "$o", "value": 1}}),
    ("sf unknown arg", {sf: {"field": "k", "input": "$o", "value": 1, "x": 1}}),
    ("sf dotted", {sf: {"field": "a.b", "input": "$o", "value": 1}}),
    ("sf value array", {sf: {"field": "k", "input": "$o", "value": [1, "$n"]}}),
    (
        "sf value literal remove?",
        {sf: {"field": "k", "input": "$o", "value": {"$literal": "$$REMOVE"}}},
    ),
    ("sf input $$ROOT", {sf: {"field": "n", "input": "$$ROOT", "value": 5}}),
    ("sf field null", {sf: {"field": None, "input": "$o", "value": 1}}),
    ("sf field empty", {sf: {"field": "", "input": "$o", "value": 1}}),
    ("sf input array", {sf: {"field": "k", "input": "$arr", "value": 1}}),
    ("sf not doc", {sf: "k"}),
    ("sf input const doc", {sf: {"field": "k", "input": {"a": "$n"}, "value": 2}}),
    ("usf basic", {"$unsetField": {"field": "k", "input": "$o"}}),
    ("usf null", {"$unsetField": {"field": "k", "input": None}}),
    ("usf missing", {"$unsetField": {"field": "k", "input": "$nope"}}),
    ("usf scalar", {"$unsetField": {"field": "k", "input": 5}}),
    ("usf field int", {"$unsetField": {"field": 5, "input": "$o"}}),
    ("usf no input", {"$unsetField": {"field": "k"}}),
    ("usf value given", {"$unsetField": {"field": "k", "input": "$o", "value": 1}}),
]

# ---- $filter
fl = "$filter"
CASES += [
    ("filter limit 1", {fl: {"input": "$arr", "cond": {"$gt": ["$$this", 1]}, "limit": 1}}),
    ("filter limit 0", {fl: {"input": "$arr", "cond": True, "limit": 0}}),
    ("filter limit neg", {fl: {"input": "$arr", "cond": True, "limit": -1}}),
    ("filter limit dbl", {fl: {"input": "$arr", "cond": True, "limit": 2.0}}),
    ("filter limit frac", {fl: {"input": "$arr", "cond": True, "limit": 1.5}}),
    ("filter limit str", {fl: {"input": "$arr", "cond": True, "limit": "a"}}),
    ("filter limit null", {fl: {"input": "$arr", "cond": True, "limit": None}}),
    ("filter limit missing", {fl: {"input": "$arr", "cond": True, "limit": "$nope"}}),
    ("filter limit long", {fl: {"input": "$arr", "cond": True, "limit": L(2)}}),
    ("filter limit long big", {fl: {"input": "$arr", "cond": True, "limit": L(2**40)}}),
    ("filter limit dec", {fl: {"input": "$arr", "cond": True, "limit": D("2")}}),
    ("filter limit field", {fl: {"input": "$arr", "cond": True, "limit": "$n"}}),
    ("filter limit nan", {fl: {"input": "$arr", "cond": True, "limit": NAN}}),
    ("filter limit bool", {fl: {"input": "$arr", "cond": True, "limit": True}}),
    ("filter limit big", {fl: {"input": "$arr", "cond": True, "limit": 100}}),
    ("filter input null", {fl: {"input": None, "cond": True}}),
    ("filter input missing", {fl: {"input": "$nope", "cond": True}}),
    ("filter input str", {fl: {"input": "$s", "cond": True}}),
    ("filter input const int", {fl: {"input": 5, "cond": True}}),
    ("filter no cond", {fl: {"input": "$arr"}}),
    ("filter no input", {fl: {"cond": True}}),
    ("filter as", {fl: {"input": "$arr", "as": "x", "cond": {"$lt": ["$$x", 3]}}}),
    ("filter bad as", {fl: {"input": "$arr", "as": "X", "cond": True}}),
    ("filter as int", {fl: {"input": "$arr", "as": 1, "cond": True}}),
    ("filter unknown", {fl: {"input": "$arr", "cond": True, "foo": 1}}),
    ("filter not doc", {fl: ["$arr"]}),
    ("filter limit null input null", {fl: {"input": None, "cond": True, "limit": 0}}),
    ("filter cond error", {fl: {"input": [], "cond": {"$divide": [1, 0]}}}),
    ("filter limit 0 input null", {fl: {"input": "$nul", "cond": True, "limit": 0}}),
]

# ---- $cmp
for lab, args in [
    ("missing vs null", ["$nope", None]),
    ("null vs missing", [None, "$nope"]),
    ("missing both", ["$nope", "$nope2"]),
    ("num vs str", [1, "a"]),
    ("nan vs nan", [NAN, NAN]),
    ("nan vs 1", [NAN, 1]),
    ("dec nan vs dbl nan", [D("NaN"), NAN]),
    ("dec vs dbl", [D("1.5"), 1.5]),
    ("long vs dbl", [L(2**53 + 1), float(2**53)]),
    ("bool vs num", [True, 1]),
    ("arr vs arr", [[1, 2], [1, 3]]),
    ("arr vs num", [[1], 1]),
    ("obj vs obj", [{"a": 1}, {"a": 1, "b": 0}]),
    ("date vs ts", [DT, Timestamp(1, 1)]),
    ("oid vs oid", [ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), ObjectId("64b7f9a2c1d2e3f4a5b6c7d9")]),
    ("minkey vs null", [MinKey(), None]),
    ("maxkey vs obj", [MaxKey(), {"a": 1}]),
    ("regex vs regex", [Regex("a", "i"), Regex("a")]),
    ("code vs code", [Code("a"), Code("b")]),
    ("code vs str", [Code("a"), "a"]),
    ("codews vs code", [Code("a", {"x": 1}), Code("a")]),
    ("bin len", [Binary(b"\x02", 0), Binary(b"\x01\x01", 0)]),
    ("bin subtype", [Binary(b"\x01", 5), Binary(b"\x01", 0)]),
    ("str unicode", ["é", "z"]),
    ("empty arr vs null", [[], None]),
    ("neg zero", [-0.0, 0]),
]:
    both(f"cmp {lab}", "$cmp", args)
CASES += [
    ("cmp one arg", {"$cmp": [1]}),
    ("cmp three", {"$cmp": [1, 2, 3]}),
    ("cmp missing vs field", {"$cmp": ["$nope", "$n"]}),
    ("cmp field vs missing", {"$cmp": ["$n", "$nope"]}),
    ("cmp arr field vs missing", {"$cmp": ["$arr", "$nope"]}),
]

# ---- $strcasecmp
for lab, args in [
    ("arr", [[1], "a"]),
    ("obj", ["a", {"a": 1}]),
    ("oid", [ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), "a"]),
    ("bin", [Binary(b"a", 0), "a"]),
    ("regex", [Regex("a"), "a"]),
    ("minkey", [MinKey(), "a"]),
    ("bool", [True, "true"]),
    ("null", [None, ""]),
    ("null vs a", [None, "a"]),
    ("date", [DT, "2020"]),
    ("ts", [Timestamp(1, 1), "a"]),
    ("dec", [D("1.50"), "1.5"]),
    ("double", [1.0, "1"]),
    ("unicode", ["É", "é"]),
    ("ascii ci", ["ABC", "abd"]),
    ("code", [Code("a"), "a"]),
    ("long", [L(5), "5"]),
]:
    both(f"strcasecmp {lab}", "$strcasecmp", args)
CASES += [
    ("strcasecmp missing", {"$strcasecmp": ["$nope", ""]}),
    ("strcasecmp one", {"$strcasecmp": ["a"]}),
    ("strcasecmp three", {"$strcasecmp": ["a", "b", "c"]}),
]

# ---- $median / $percentile
for op in ["$median", "$percentile"]:
    base = {"method": "approximate"}
    if op == "$percentile":
        base["p"] = [0.5, 0.9]

    def sp(base=base, **kw):
        d = dict(base)
        d.update(kw)
        return d

    CASES += [
        (f"{op} arr", {op: sp(input="$arr")}),
        (f"{op} scalar", {op: sp(input="$n")}),
        (f"{op} null", {op: sp(input=None)}),
        (f"{op} missing", {op: sp(input="$nope")}),
        (f"{op} str", {op: sp(input="$s")}),
        (f"{op} mixed", {op: sp(input=[1, "a", None, 2.5, D("3"), L(4), True])}),
        (f"{op} empty", {op: sp(input=[])}),
        (f"{op} nan", {op: sp(input=[NAN, 1, 2])}),
        (f"{op} inf", {op: sp(input=[INF, 1, -INF])}),
        (f"{op} dec only", {op: sp(input=[D("1.5"), D("2.5")])}),
        (f"{op} method bad", {op: dict(sp(input="$arr"), method="exact")}),
        (
            f"{op} method missing",
            {op: {k: v for k, v in sp(input="$arr").items() if k != "method"}},
        ),
        (f"{op} method int", {op: dict(sp(input="$arr"), method=1)}),
        (f"{op} no input", {op: {k: v for k, v in base.items()}}),
        (f"{op} unknown", {op: dict(sp(input="$arr"), x=1)}),
        (f"{op} not doc", {op: "$arr"}),
        (f"{op} nested arr", {op: sp(input=[[1, 2], 3])}),
        (f"{op} big long", {op: sp(input=[L(2**62), 1])}),
        (f"{op} input expr", {op: sp(input={"$concatArrays": ["$arr", [10]]})}),
        (f"{op} input dates", {op: sp(input=[DT, 1])}),
    ]
pc = "$percentile"
CASES += [
    ("pct p empty", {pc: {"input": "$arr", "p": [], "method": "approximate"}}),
    ("pct p scalar", {pc: {"input": "$arr", "p": 0.5, "method": "approximate"}}),
    ("pct p >1", {pc: {"input": "$arr", "p": [1.5], "method": "approximate"}}),
    ("pct p neg", {pc: {"input": "$arr", "p": [-0.1], "method": "approximate"}}),
    ("pct p str", {pc: {"input": "$arr", "p": ["a"], "method": "approximate"}}),
    ("pct p 0 1", {pc: {"input": "$arr", "p": [0, 1], "method": "approximate"}}),
    ("pct p int 1", {pc: {"input": "$arr", "p": [1], "method": "approximate"}}),
    ("pct p dec", {pc: {"input": "$arr", "p": [D("0.5")], "method": "approximate"}}),
    ("pct p field", {pc: {"input": "$arr", "p": "$arr", "method": "approximate"}}),
    (
        "pct p expr elems",
        {pc: {"input": "$arr", "p": [{"$divide": [1, 2]}], "method": "approximate"}},
    ),
    ("pct p nan", {pc: {"input": "$arr", "p": [NAN], "method": "approximate"}}),
    ("pct p missing", {pc: {"input": "$arr", "method": "approximate"}}),
    ("pct p long", {pc: {"input": "$arr", "p": [L(0)], "method": "approximate"}}),
    ("pct p null", {pc: {"input": "$arr", "p": None, "method": "approximate"}}),
    ("pct p literal", {pc: {"input": "$arr", "p": {"$literal": [0.25]}, "method": "approximate"}}),
    ("median p given", {"$median": {"input": "$arr", "p": [0.5], "method": "approximate"}}),
    (
        "pct many",
        {
            pc: {
                "input": [5, 1, 4, 2, 3, 9, 8, 7, 6, 10],
                "p": [0.0, 0.1, 0.11, 0.25, 0.5, 0.95, 0.999, 1.0],
                "method": "approximate",
            }
        },
    ),
]

# ---- $meta
for k in [
    "textScore",
    "geoNearDistance",
    "geoNearPoint",
    "score",
    "scoreDetails",
    "sortKey",
    "searchScore",
    "searchHighlights",
    "indexKey",
    "recordId",
    "randVal",
    "vectorSearchScore",
    "searchScoreDetails",
    "searchSequenceToken",
    "bogus",
    5,
    "",
    None,
]:
    CASES.append((f"meta {k!r}", {"$meta": k}))
CASES.append(("meta field path", {"$meta": "$s"}))


D = Decimal128
L = Int64
NAN = float("nan")
INF = float("inf")
DT = datetime.datetime(2020, 1, 1)


def conv(label, value, to, **kw):
    spec = {"input": lit(value), "to": to}
    spec.update(kw)
    CASES.append((f"cv {label} [const]", {"$convert": spec}))
    spec2 = {"input": "$v", "to": to}
    spec2.update(kw)
    CASES.append((f"cv {label} [field]", {"$convert": spec2}, {"_id": 1, "v": value}))


# string -> double edge parses
for s in [
    "1.5",
    "  1.5",
    "1.5  ",
    "+1.5",
    "-1.5",
    ".5",
    "5.",
    ".",
    "1e5",
    "1E+5",
    "1e",
    "1e+",
    "1.5e-3x",
    "inf",
    "-inf",
    "Infinity",
    "-Infinity",
    "INF",
    "nan",
    "NaN",
    "-nan",
    "infinity!",
    "nanx",
    "0x1p3",
    "0x1.8p1",
    "-0x10",
    "0X1A",
    "0x",
    "0x1.",
    "0x.8",
    "0x1p",
    "0xg",
    "1e400",
    "-1e400",
    "1e-400",
    "4.9e-324",
    "abc",
    "",
    "1,5",
    "١",
    "1_000",
    "0001",
    "1e5.5",
    "+",
    "-",
    "+.e1",
    "2.2250738585072014e-308",
    "1.7976931348623157e308",
    "1.8e308",
    "0x1p1024",
    "0x1P-1074",
    "  ",
    "\t1",
    "1\n",
]:
    conv(f"str2dbl {s!r}", s, "double")
    conv(f"str2dec {s!r}", s, "decimal")
for s in [
    "5",
    "+5",
    "-5",
    " 5",
    "5 ",
    "0x10",
    "1.0",
    "1e3",
    "2147483648",
    "-2147483649",
    "9223372036854775808",
    "99999999999999999999999999999999999999999",
    "",
    "abc",
    "007",
    "-0",
    "+-1",
]:
    conv(f"str2int {s!r}", s, "int")
    conv(f"str2long {s!r}", s, "long")
# decimal edge strings
for s in [
    "1E+6145",
    "1E+6144",
    "1E-6176",
    "1E-6177",
    "123456789012345678901234567890123456789",
    "1.234567890123456789012345678901234567E-6170",
    "0E+99999",
    "0E-99999",
    "-0",
    "1e99999999999999999999",
    "1e-99999999999999999999",
    "9.999999999999999999999999999999999E+6144",
    "99999999999999999999999999999999999E+6111",
    "1E+6111",
    "10E+6111",
    "Inf",
    "-NaN",
    "SNaN",
    "1.0E+2",
    "1.",
    ".1E3",
]:
    conv(f"str2dec {s!r}", s, "decimal")

# format / byteOrder / subtype options
for f in ["auto", "base64", "base64url", "hex", "utf8", "uuid", "bogus", 5, None, "BASE64"]:
    conv(f"bin2str fmt {f!r}", Binary(b"\x01\x02\xff", 0), "string", format=f)
    conv(f"str2bin fmt {f!r}", "AQL/", "binData", format=f)
for f in ["base64", "base64url"]:
    for s in [
        "AQ==",
        "AQ",
        "AQ=",
        "A",
        "AQID",
        "AQ==AQ==",
        "A===",
        "-_8=",
        "+/8=",
        "AQ!D",
        "",
        "====",
        "AB=C",
        "QUJD\n",
        "QUJ",
    ]:
        conv(f"str2bin {f} {s!r}", s, "binData", format=f)
for s in ["0a0B", "0g", "abc", "", "ZZ", "00ff"]:
    conv(f"str2bin hex {s!r}", s, "binData", format="hex")
for s in [
    "123e4567-e89b-12d3-a456-426614174000",
    "123e4567e89b12d3a456426614174000",
    "123E4567-E89B-12D3-A456-426614174000",
    "123e4567-e89b-12d3-a456-42661417400g",
    "x",
]:
    conv(f"str2bin uuid {s!r}", s, {"type": "binData", "subtype": 4}, format="uuid")
    conv(f"str2bin uuid sub0 {s!r}", s, "binData", format="uuid")
for bo in ["little", "big", "BIG", 1, None, "middle"]:
    conv(f"int2bin bo {bo!r}", 258, "binData", byteOrder=bo)
    conv(f"long2bin bo {bo!r}", L(258), "binData", byteOrder=bo)
    conv(f"dbl2bin bo {bo!r}", 1.5, "binData", byteOrder=bo)
    conv(f"bin2int bo {bo!r}", Binary(b"\x01\x02", 0), "int", byteOrder=bo)
    conv(
        f"bin2long8 bo {bo!r}", Binary(b"\x01\x02\x03\x04\x05\x06\x07\x08", 0), "long", byteOrder=bo
    )
    conv(
        f"bin2dbl8 bo {bo!r}",
        Binary(b"\x00\x00\x00\x00\x00\x00\xf0\x3f", 0),
        "double",
        byteOrder=bo,
    )
    conv(f"bin2dbl4 bo {bo!r}", Binary(b"\x00\x00\x80\x3f", 0), "double", byteOrder=bo)
for sub in [
    0,
    4,
    5,
    9,
    10,
    127,
    128,
    255,
    256,
    -1,
    1.0,
    1.5,
    L(5),
    D("5"),
    D("5.5"),
    "5",
    None,
    NAN,
    INF,
]:
    conv(
        f"str2bin subtype {type(sub).__name__} {sub!r}",
        "AQID",
        {"type": "binData", "subtype": sub},
        format="base64",
    )
for v in [
    Binary(b"\x01", 0),
    Binary(b"\x01\x02", 0),
    Binary(b"\x01\x02\x03", 0),
    Binary(b"\x01\x02\x03\x04", 0),
    Binary(b"\xff" * 8, 0),
    Binary(b"", 0),
    Binary(b"\x80", 5),
]:
    for to in ["int", "long", "double", "decimal", "bool", "date", "string", "objectId", "binData"]:
        conv(f"bin{len(v)}/{v.subtype} to {to}", v, to, format="hex") if to == "string" else conv(
            f"bin{len(v)}/{v.subtype} to {to}", v, to
        )
for v in [
    1,
    L(1),
    1.5,
    D("1"),
    True,
    DT,
    ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"),
    Timestamp(1, 1),
    [1],
    {"a": 1},
    Regex("a"),
    MinKey(),
    Code("x"),
]:
    conv(f"{type(v).__name__} to binData", v, "binData")
# to target forms
for to in [
    {"type": "int"},
    {"type": 16},
    {"type": "binData"},
    {"type": "binData", "subtype": 0},
    {"type": "int", "subtype": 4},
    {"type": "bogus"},
    {"subtype": 0},
    {},
    {"type": "int", "x": 1},
    "bogus",
    99,
    3,
    4,
    6,
    10,
    1.0,
    L(16),
    D("16"),
    16.5,
    None,
    True,
    ["int"],
    "missing",
    "javascript",
    "regex",
    "null",
    "array",
    "object",
    "timestamp",
    "minKey",
    "number",
    "bool",
    "objectId",
]:
    conv(f"to {to!r}", 5, to)
# onNull / onError
CASES += [
    ("cv onNull", {"$convert": {"input": None, "to": "int", "onNull": "N"}}),
    ("cv onNull missing input", {"$convert": {"input": "$nope", "to": "int", "onNull": "N"}}),
    ("cv onNull to bad", {"$convert": {"input": None, "to": "bogus", "onNull": "N"}}),
    ("cv onNull to null", {"$convert": {"input": 1, "to": None}}),
    ("cv onNull to missing", {"$convert": {"input": 1, "to": "$nope"}}),
    ("cv onError unsupported", {"$convert": {"input": [1], "to": "int", "onError": "E"}}),
    ("cv onError parse", {"$convert": {"input": "x", "to": "int", "onError": "E"}}),
    (
        "cv onError bad format",
        {"$convert": {"input": "x", "to": "binData", "format": "bogus", "onError": "E"}},
    ),
    ("cv onError bad target", {"$convert": {"input": "x", "to": "bogus", "onError": "E"}}),
    ("cv onError missing path", {"$convert": {"input": "x", "to": "int", "onError": "$nope"}}),
    (
        "cv onError bad byteOrder",
        {"$convert": {"input": 1, "to": "binData", "byteOrder": "x", "onError": "E"}},
    ),
    (
        "cv onError bad subtype",
        {
            "$convert": {
                "input": "AQ==",
                "to": {"type": "binData", "subtype": 300},
                "format": "base64",
                "onError": "E",
            }
        },
    ),
    ("cv no to", {"$convert": {"input": 1}}),
    ("cv no input", {"$convert": {"to": "int"}}),
    ("cv unknown", {"$convert": {"input": 1, "to": "int", "x": 1}}),
    ("cv not doc", {"$convert": [1, "int"]}),
    ("cv to field", {"$convert": {"input": "$n", "to": "$s"}}),
    ("cv uuid toUUID", {"$toUUID": "123e4567-e89b-12d3-a456-426614174000"}),
    (
        "cv toUUID bad",
        {"$toUUID": "x"},
    ),
    # {$toUUID: 5} is left out: both servers send a reply pymongo cannot
    # decode (InvalidBSON).
    ("cv toUUID arr", {"$toUUID": ["123e4567-e89b-12d3-a456-426614174000"]}),
    ("cv toUUID null", {"$toUUID": None}),
    (
        "cv bin2str auto uuid",
        {
            "$convert": {
                "input": Binary(uuid.UUID("123e4567-e89b-12d3-a456-426614174000").bytes, 4),
                "to": "string",
                "format": "auto",
            }
        },
    ),
    (
        "cv bin2str uuid sub0",
        {"$convert": {"input": Binary(b"\x01" * 16, 0), "to": "string", "format": "uuid"}},
    ),
    (
        "cv bin2str utf8 invalid",
        {"$convert": {"input": Binary(b"\xff\xfe", 0), "to": "string", "format": "utf8"}},
    ),
    (
        "cv bin2str utf8 ok",
        {"$convert": {"input": Binary("héllo".encode(), 0), "to": "string", "format": "utf8"}},
    ),
    ("cv bin2str no format", {"$convert": {"input": Binary(b"\x01", 0), "to": "string"}}),
    ("cv toString bin", {"$toString": Binary(b"\x01\x02", 0)}),
    ("cv toString uuid", {"$toString": Binary(b"\x01" * 16, 4)}),
    ("cv str2bin no format", {"$convert": {"input": "AQ==", "to": "binData"}}),
    ("cv bin2bin same", {"$convert": {"input": Binary(b"\x01", 0), "to": "binData"}}),
    ("cv bin2bin diff", {"$convert": {"input": Binary(b"\x01", 5), "to": "binData"}}),
    (
        "cv bin2bin diff sub",
        {"$convert": {"input": Binary(b"\x01", 5), "to": {"type": "binData", "subtype": 5}}},
    ),
    ("cv str2bin utf8", {"$convert": {"input": "héllo", "to": "binData", "format": "utf8"}}),
    ("cv str2bin auto", {"$convert": {"input": "AQ==", "to": "binData", "format": "auto"}}),
    ("cv dbl2bin big", {"$convert": {"input": 1.0, "to": "binData", "byteOrder": "big"}}),
    ("cv bool2bin", {"$convert": {"input": True, "to": "binData"}}),
    ("cv dec2dbl tiny", {"$convert": {"input": D("1E-400"), "to": "double"}}),
    ("cv dec2dbl huge", {"$convert": {"input": D("1E+400"), "to": "double"}}),
    ("cv dec2dbl denorm", {"$convert": {"input": D("1E-310"), "to": "double"}}),
    ("cv dec2dbl -0", {"$convert": {"input": D("-0"), "to": "double"}}),
    ("cv dec2dbl -inf", {"$convert": {"input": D("-Infinity"), "to": "double"}}),
    ("cv dec2dbl neg tiny", {"$convert": {"input": D("-1E-400"), "to": "double"}}),
    ("cv dec2date huge", {"$convert": {"input": D("1E+30"), "to": "date"}}),
    ("cv dec2date frac", {"$convert": {"input": D("1.9"), "to": "date"}}),
    ("cv dec2date neg frac", {"$convert": {"input": D("-1.9"), "to": "date"}}),
    ("cv dbl2date frac", {"$convert": {"input": 1.9, "to": "date"}}),
    ("cv dbl2date neg frac", {"$convert": {"input": -1.9, "to": "date"}}),
    ("cv dbl2date huge", {"$convert": {"input": 1e30, "to": "date"}}),
    ("cv str2date bad", {"$convert": {"input": "notadate", "to": "date"}}),
    ("cv str2date ok", {"$convert": {"input": "2020-01-02T03:04:05Z", "to": "date"}}),
    ("cv dec2int round", {"$convert": {"input": D("2.9"), "to": "int"}}),
    ("cv dec2int neg", {"$convert": {"input": D("-2.9"), "to": "int"}}),
    ("cv dec2long huge", {"$convert": {"input": D("9223372036854775808"), "to": "long"}}),
    ("cv dec2int big", {"$convert": {"input": D("2147483648"), "to": "int"}}),
    ("cv dbl2long edge", {"$convert": {"input": 9.223372036854776e18, "to": "long"}}),
    ("cv dbl2long -edge", {"$convert": {"input": -9.223372036854776e18, "to": "long"}}),
    ("cv dbl2int edge", {"$convert": {"input": 2147483647.9, "to": "int"}}),
    ("cv dbl2int over", {"$convert": {"input": 2147483648.0, "to": "int"}}),
    ("cv long2int over", {"$convert": {"input": L(2**31), "to": "int"}}),
    ("cv date2int", {"$convert": {"input": DT, "to": "int"}}),
    ("cv bool2dec", {"$convert": {"input": False, "to": "decimal"}}),
    ("cv dbl2dec", {"$convert": {"input": 0.1, "to": "decimal"}}),
    ("cv dbl2dec big", {"$convert": {"input": 1e300, "to": "decimal"}}),
    ("cv dbl2dec tiny", {"$convert": {"input": 5e-324, "to": "decimal"}}),
    ("cv str2oid upper", {"$convert": {"input": "64B7F9A2C1D2E3F4A5B6C7D8", "to": "objectId"}}),
    ("cv str2oid bad char", {"$convert": {"input": "64b7f9a2c1d2e3f4a5b6c7dz", "to": "objectId"}}),
    (
        "cv str2oid unicode len",
        {"$convert": {"input": "64b7f9a2c1d2e3f4a5b6c7ée", "to": "objectId"}},
    ),
    ("cv ts2date", {"$convert": {"input": Timestamp(5, 1), "to": "date"}}),
    ("cv oid2date", {"$convert": {"input": ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), "to": "date"}}),
    ("cv ts2long", {"$convert": {"input": Timestamp(5, 1), "to": "long"}}),
    ("cv ts2str", {"$convert": {"input": Timestamp(5, 1), "to": "string"}}),
    ("cv regex2str", {"$convert": {"input": Regex("a", "i"), "to": "string"}}),
    ("cv code2str", {"$convert": {"input": Code("x"), "to": "string"}}),
    ("cv minkey2bool", {"$convert": {"input": MinKey(), "to": "bool"}}),
    ("cv arr2bool", {"$convert": {"input": [], "to": "bool"}}),
    ("cv dec nan2bool", {"$convert": {"input": D("NaN"), "to": "bool"}}),
    ("cv dec -0 2bool", {"$convert": {"input": D("-0"), "to": "bool"}}),
    ("cv nan2bool", {"$convert": {"input": NAN, "to": "bool"}}),
    ("cv dbl2str", {"$convert": {"input": 1e21, "to": "string"}}),
    ("cv dbl2str small", {"$convert": {"input": 1e-7, "to": "string"}}),
    ("cv dbl2str -0", {"$convert": {"input": -0.0, "to": "string"}}),
    ("cv dec2str", {"$convert": {"input": D("1.50E+3"), "to": "string"}}),
    ("cv date2str old", {"$convert": {"input": datetime.datetime(1, 1, 1), "to": "string"}}),
    ("cv format on int", {"$convert": {"input": 5, "to": "int", "format": "base64"}}),
    ("cv format on int bad", {"$convert": {"input": 5, "to": "int", "format": "bogus"}}),
    ("cv byteOrder on str", {"$convert": {"input": "5", "to": "int", "byteOrder": "big"}}),
]


D = Decimal128
L = Int64
NAN = float("nan")
INF = float("inf")
DT = datetime.datetime(2020, 1, 1)


def both(label, op, args):
    CASES.append((label + " [const]", {op: [lit(a) for a in args]}))
    doc = {"_id": 1}
    fargs = []
    for i, a in enumerate(args):
        doc[f"x{i}"] = a
        fargs.append(f"$x{i}")
    CASES.append((label + " [field]", {op: fargs}, doc))


# ---- regex family
for op in ["$regexFind", "$regexFindAll", "$regexMatch"]:
    for lab, spec in [
        ("basic", {"input": "abcabc", "regex": "b"}),
        ("captures", {"input": "a1b2c3", "regex": "([a-z])(\\d)"}),
        ("optional capture", {"input": "ac", "regex": "a(b)?c"}),
        ("unicode idx", {"input": "héllo héllo", "regex": "l+"}),
        ("emoji idx", {"input": "😀a😀a", "regex": "a"}),
        ("empty match", {"input": "abc", "regex": ""}),
        ("empty match star", {"input": "abc", "regex": "x*"}),
        ("no match", {"input": "abc", "regex": "z"}),
        ("options i", {"input": "ABC", "regex": "b", "options": "i"}),
        ("options x", {"input": "abc", "regex": "a b # c", "options": "x"}),
        ("options s", {"input": "a\nb", "regex": "a.b", "options": "s"}),
        ("options m", {"input": "a\nb", "regex": "^b", "options": "m"}),
        ("options u", {"input": "abc", "regex": "b", "options": "u"}),
        ("options bad", {"input": "abc", "regex": "b", "options": "z"}),
        ("options int", {"input": "abc", "regex": "b", "options": 1}),
        ("options null", {"input": "abc", "regex": "b", "options": None}),
        ("regex obj", {"input": "ABC", "regex": Regex("b", "i")}),
        ("regex obj + options", {"input": "ABC", "regex": Regex("b", "i"), "options": "m"}),
        ("regex obj no flags + options", {"input": "ABC", "regex": Regex("b"), "options": "i"}),
        ("regex null", {"input": "abc", "regex": None}),
        ("regex int", {"input": "abc", "regex": 5}),
        ("input int", {"input": 5, "regex": "5"}),
        ("input null", {"input": None, "regex": "a"}),
        ("input missing", {"input": "$nope", "regex": "a"}),
        ("regex missing", {"input": "abc", "regex": "$nope"}),
        ("no regex", {"input": "abc"}),
        ("no input", {"regex": "a"}),
        ("unknown", {"input": "abc", "regex": "a", "x": 1}),
        ("bad pattern", {"input": "abc", "regex": "("}),
        ("bad pattern field", {"input": "abc", "regex": "$s"}),
        ("nul in pattern", {"input": "a\x00b", "regex": "a\x00b"}),
        ("nul in options", {"input": "abc", "regex": "a", "options": "i\x00"}),
        ("lookbehind", {"input": "xab", "regex": "(?<=a)b"}),
        ("backref", {"input": "aa", "regex": "(a)\\1"}),
        ("named group", {"input": "ab", "regex": "(?<n>a)b"}),
        ("possessive", {"input": "aaa", "regex": "a++"}),
        ("unicode class", {"input": "é1", "regex": "\\w+"}),
        ("long input", {"input": "ab" * 50, "regex": "b"}),
        ("overlap", {"input": "aaaa", "regex": "aa"}),
        ("empty input", {"input": "", "regex": "^$"}),
        ("options dup", {"input": "ABC", "regex": "b", "options": "ii"}),
        ("regex obj bad flag", {"input": "abc", "regex": Regex("b", "l")}),
        ("utf8 nonchar idx", {"input": "aé́b", "regex": "b"}),
        ("dot unicode", {"input": "😀", "regex": "^.$"}),
        ("input regex type", {"input": Regex("a"), "regex": "a"}),
        ("input arr", {"input": ["a"], "regex": "a"}),
        ("pattern expr", {"input": "abc", "regex": {"$concat": ["b", "c"]}}),
        ("options expr", {"input": "ABC", "regex": "b", "options": {"$concat": ["i"]}}),
        ("pattern with flags str", {"input": "ABC", "regex": "/b/i"}),
    ]:
        CASES.append((f"{op} {lab}", {op: spec}))
    CASES += [
        (f"{op} not doc", {op: "abc"}),
        (f"{op} field input", {op: {"input": "$u", "regex": "ö"}}),
        (f"{op} field regex", {op: {"input": "$s", "regex": "$re"}}),
        (f"{op} field regex options", {op: {"input": "$s", "regex": "$re", "options": "m"}}),
        (f"{op} empty match unicode", {op: {"input": "é😀", "regex": ""}}),
    ]

# ---- py_eq via $eq/$ne/$in/$indexOfArray/$setEquals
vals = [
    ("code", Code("a")),
    ("code2", Code("b")),
    ("cws", Code("a", {"x": 1})),
    ("cws2", Code("a", {"x": 2})),
    ("str a", "a"),
    ("dec1", D("1")),
    ("dec nan", D("NaN")),
    ("dbl nan", NAN),
    ("int1", 1),
    ("bool t", True),
    ("dec1.0", D("1.0")),
    ("oid", ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")),
    ("date", DT),
    ("ts", Timestamp(1, 1)),
    ("ts2", Timestamp(1, 2)),
    ("re", Regex("a")),
    ("re i", Regex("a", "i")),
    ("bin0", Binary(b"\x01", 0)),
    ("bin5", Binary(b"\x01", 5)),
    ("null", None),
    ("min", MinKey()),
    ("max", MaxKey()),
    ("arr dec", [D("1")]),
    ("arr 1", [1.0]),
    ("doc", {"a": D("1")}),
    ("doc2", {"a": 1}),
    ("doc order", {"b": 1, "a": 1}),
    ("doc order2", {"a": 1, "b": 1}),
    ("neg0", -0.0),
    ("zero", 0),
    ("dec -0", D("-0")),
    ("long", L(1)),
    ("date0", datetime.datetime(1970, 1, 1)),
    ("ts0", Timestamp(0, 0)),
]
pairs = [
    ("code", "code"),
    ("code", "code2"),
    ("code", "str a"),
    ("cws", "cws"),
    ("cws", "cws2"),
    ("cws", "code"),
    ("dec1", "int1"),
    ("dec1", "bool t"),
    ("dec nan", "dbl nan"),
    ("dec nan", "dec nan"),
    ("dbl nan", "dbl nan"),
    ("dec1", "dec1.0"),
    ("dec1", "str a"),
    ("dec1", "code"),
    ("oid", "oid"),
    ("date", "date"),
    ("ts", "ts2"),
    ("ts", "ts"),
    ("re", "re i"),
    ("re", "re"),
    ("bin0", "bin5"),
    ("null", "null"),
    ("min", "min"),
    ("max", "min"),
    ("arr dec", "arr 1"),
    ("doc", "doc2"),
    ("doc order", "doc order2"),
    ("neg0", "zero"),
    ("dec -0", "zero"),
    ("long", "dec1"),
    ("bool t", "int1"),
    ("date0", "ts0"),
    ("date0", "zero"),
    ("min", "null"),
    ("re", "str a"),
    ("code", "re"),
]
V = dict(vals)
for a, b in pairs:
    both(f"eq {a}~{b}", "$eq", [V[a], V[b]])
    CASES.append((f"in {a}~{b}", {"$in": [lit(V[a]), {"$literal": [V[b]]}]}))
    CASES.append((f"indexOfArray {a}~{b}", {"$indexOfArray": [{"$literal": [0, V[b]]}, lit(V[a])]}))
    CASES.append((f"lt {a}~{b}", {"$lt": [lit(V[a]), lit(V[b])]}))
CASES += [
    ("setEquals code", {"$setEquals": [[Code("a")], [Code("a")]]}),
    ("setEquals cws", {"$setEquals": [[Code("a", {"x": 1})], [Code("a", {"x": 2})]]}),
    ("setUnion code", {"$setUnion": [[Code("a")], [Code("a"), Code("b")]]}),
    ("setUnion bin", {"$setUnion": [[Binary(b"\x01", 0)], [Binary(b"\x01", 5)]]}),
    ("setIntersection nan", {"$setIntersection": [[NAN, D("NaN")], [D("NaN")]]}),
]

# ---- index_of_pos via $indexOfArray / $indexOfCP / $indexOfBytes
for op, hay, needle in [
    ("$indexOfArray", [1, 2, 3, 2], 2),
    ("$indexOfCP", "abcb", "b"),
    ("$indexOfBytes", "abcb", "b"),
]:
    for lab, extra in [
        ("neg start", [-1]),
        ("neg end", [0, -1]),
        ("dec start", [D("1")]),
        ("dec frac start", [D("1.5")]),
        ("dec nan", [D("NaN")]),
        ("dbl start", [2.0]),
        ("dbl frac", [1.5]),
        ("long start", [L(2)]),
        ("long huge", [L(2**40)]),
        ("str start", ["1"]),
        ("null start", [None]),
        ("bool start", [True]),
        ("big end", [0, 2**31 - 1]),
        ("end < start", [3, 1]),
        ("start past", [10]),
        ("dbl huge", [3e9]),
        ("dec neg", [D("-1")]),
        ("neg zero", [-0.0]),
        ("inf", [INF]),
        ("null end", [0, None]),
        ("str end", [0, "x"]),
        ("dec end", [0, D("2")]),
    ]:
        both(f"{op} {lab}", op, [hay, needle] + extra)
CASES += [
    ("indexOfArray missing start", {"$indexOfArray": ["$arr", 1, "$nope"]}),
    ("indexOfCP missing start", {"$indexOfCP": ["$s", "b", "$nope"]}),
]


D = Decimal128
L = Int64
NAN = float("nan")
INF = float("inf")


for lab, a, b in [
    ("max~max", MaxKey(), MaxKey()),
    ("min~max", MinKey(), MaxKey()),
    ("min field", "$lo", MinKey()),
    ("max field", "$hi", "$hi"),
    ("arr min", [MinKey()], [MinKey()]),
    ("doc min", {"a": MinKey()}, {"a": MinKey()}),
    ("ne min", None, None),
]:
    if lab == "ne min":
        CASES.append(("ne min~min", {"$ne": [MinKey(), MinKey()]}))
        continue
    CASES.append(
        (
            f"eq2 {lab}",
            {
                "$eq": [
                    lit(a) if not isinstance(a, str) else a,
                    lit(b) if not isinstance(b, str) else b,
                ]
            },
        )
    )
CASES += [
    (
        "eq2 doc order nested",
        {"$eq": [{"$literal": {"x": {"b": 1, "a": 1}}}, {"$literal": {"x": {"a": 1, "b": 1}}}]},
    ),
    ("eq2 doc order field", {"$eq": ["$o", {"$literal": {"j": {"x": 1}, "k": 1}}]}),
    ("in doc order field", {"$in": [{"$literal": {"j": {"x": 1}, "k": 1}}, ["$o"]]}),
    ("setEquals doc order", {"$setEquals": [[{"a": 1, "b": 1}], [{"$literal": {"b": 1, "a": 1}}]]}),
    ("in maxkey", {"$in": [MaxKey(), [MaxKey()]]}),
    ("gte min min", {"$gte": [MinKey(), MinKey()]}),
    ("cmp min min", {"$cmp": [MinKey(), MinKey()]}),
]
# convert double->int boundaries
for v in [
    2147483647.5,
    2147483647.0,
    -2147483648.9,
    -2147483648.0,
    -2147483649.0,
    9.223372036854775e18,
    -9.223372036854775807e18,
    1e19,
]:
    CASES.append((f"cv2 dbl2int {v!r}", {"$convert": {"input": v, "to": "int"}}))
    CASES.append((f"cv2 dbl2long {v!r}", {"$convert": {"input": v, "to": "long"}}))
    CASES.append((f"cv2 toInt {v!r}", {"$toInt": v}))
for s in [
    "2.2250738585072011e-308",
    "2.2250738585072014e-308",
    "1e-320",
    "-1e-400",
    "0e-400",
    "0.0",
    "1e-307",
]:
    CASES.append((f"cv2 str2dbl {s}", {"$convert": {"input": s, "to": "double"}}))
    CASES.append((f"cv2 toDouble {s}", {"$toDouble": s}))
for ms in [
    -62135596800000,
    -62135596800001,
    -1,
    -86400000 * 365 * 100,
    253402300799999,
    253402300800000,
    -(2**53),
]:
    CASES.append((f"cv2 date2str {ms}", {"$toString": "$t"}, {"_id": 1, "t": DatetimeMS(ms)}))
    CASES.append(
        (f"cv2 date2str const {ms}", {"$convert": {"input": {"$toDate": L(ms)}, "to": "string"}})
    )
# $switch more
sw = "$switch"
CASES += [
    ("sw2 field all false no default", {sw: {"branches": [{"case": "$nul", "then": 1}]}}),
    ("sw2 const true", {sw: {"branches": [{"case": True, "then": "$n"}]}}),
    ("sw2 branches null", {sw: {"branches": None}}),
    ("sw2 default only const false", {sw: {"branches": [{"case": 0, "then": 1}], "default": 2}}),
    (
        "sw2 then const error const case",
        {sw: {"branches": [{"case": True, "then": {"$divide": [1, 0]}}]}},
    ),
    ("sw2 case const error", {sw: {"branches": [{"case": {"$divide": [1, 0]}, "then": 1}]}}),
    (
        "sw2 field case then error",
        {sw: {"branches": [{"case": "$n", "then": {"$divide": [1, 0]}}], "default": 0}},
    ),
]
# $filter / $map const-fold family
CASES += [
    ("ff cond error const nonempty", {"$filter": {"input": [1], "cond": {"$divide": [1, 0]}}}),
    ("ff cond error field", {"$filter": {"input": "$arr", "cond": {"$divide": [1, 0]}}}),
    ("ff cond error field empty", {"$filter": {"input": "$nul", "cond": {"$divide": [1, 0]}}}),
    ("map in error const empty", {"$map": {"input": [], "in": {"$divide": [1, 0]}}}),
    ("cond else error const", {"$cond": [True, 1, {"$divide": [1, 0]}]}),
    ("ifNull error const", {"$ifNull": [1, {"$divide": [1, 0]}]}),
    ("and short const", {"$and": [False, {"$divide": [1, 0]}]}),
    (
        "reduce error const empty",
        {"$reduce": {"input": [], "initialValue": 0, "in": {"$divide": [1, 0]}}},
    ),
    ("zip limit filter 0 const", {"$filter": {"input": [1, 2], "cond": True, "limit": 0}}),
    ("ff limit 0 empty", {"$filter": {"input": [], "cond": True, "limit": 0}}),
    ("ff limit 0 null input", {"$filter": {"input": None, "cond": True, "limit": 0}}),
    ("ff limit dbl field", {"$filter": {"input": "$arr", "cond": True, "limit": "$d"}}),
]

# Unicode classes and empty matches (regexutil): \w / \d / \s / \b without and with the u flag
for _op in ["$regexMatch", "$regexFind"]:
    for _lab, _spec in [
        ("w é", {"input": "é", "regex": "^\\w$"}),
        ("w é u", {"input": "é", "regex": "^\\w$", "options": "u"}),
        ("d arabic", {"input": "١", "regex": "^\\d$"}),
        ("s nbsp", {"input": " ", "regex": "^\\s$"}),
        ("b unicode", {"input": "éa", "regex": "\\ba"}),
        ("i unicode", {"input": "É", "regex": "é", "options": "i"}),
        ("dot newline", {"input": "\n", "regex": "^.$"}),
        ("dollar before nl", {"input": "a\n", "regex": "a$"}),
    ]:
        CASES.append((f"{_op} uc {_lab}", {_op: _spec}))
CASES += [
    ("$regexFindAll trailing empty after match", {"$regexFindAll": {"input": "ab", "regex": "b*"}}),
    ("$regexFindAll empty on empty", {"$regexFindAll": {"input": "", "regex": ""}}),
]
