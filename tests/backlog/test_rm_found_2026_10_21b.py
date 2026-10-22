"""Gap tests for the expression divergences found on 2026-10-21 by probing the
lines of `secantus-core/src/expressions.rs` that no test executed.

A line-coverage run (`docs-rust/coverage.md`) showed ~1,200 such lines even
after the generic `tools/probes/agg_expressions.py` sweep; `op_switch` had
never run at all. `tools/probes/expression_shapes.py` holds the cases written
against them. Each test here holds the shapes of one root cause and asks
mongod live, so the expected answer is mongod's on whatever build CI runs.
"""

from __future__ import annotations

import datetime
from typing import Any

import pytest
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, Regex, json_util
from bson.codec_options import CodecOptions, DatetimeConversion

from tests.backlog.conftest import mongo_answer

_CANONICAL = json_util.JSONOptions(json_mode=json_util.JSONMode.CANONICAL)
_CODEC = CodecOptions(datetime_conversion=DatetimeConversion.DATETIME_AUTO)

T = datetime.datetime(2026, 1, 2, 3, 4, 5, 678000)
DOC = {"_id": 1, "n": 2, "s": "abc", "t": T, "arr": [3, 1, 2], "nul": None}


def _coll(client: Any, name: str = "x") -> Any:
    return client.get_database("rm_found_1021b", codec_options=_CODEC)[name]


def _answer(client: Any, expr: Any, doc: dict, where: str) -> str:
    """One server's answer to `expr` over `doc`: the value (canonical extended
    JSON, so a type difference is a difference) or the error CODE. The error
    message is not compared: these items are about answers and codes."""
    coll = _coll(client)
    coll.drop()
    coll.insert_one(doc)
    if where == "$match":
        run = lambda: len(list(coll.aggregate([{"$match": {"$expr": {"$eq": [expr, "\x00"]}}}])))  # noqa: E731
    elif where == "find":
        run = lambda: list(coll.find({}, {"_id": 0, "z": expr}))  # noqa: E731
    else:
        run = lambda: list(coll.aggregate([{"$project": {"_id": 0, "z": expr}}]))  # noqa: E731
    got = mongo_answer(run)
    if got[0] == "err":
        return f"error {got[1]}"
    return json_util.dumps(got[1], json_options=_CANONICAL)


def _diverging(rust_mongo: Any, mongod: Any, shapes: list[tuple]) -> list[str]:
    """The shapes whose answers differ, each with both answers."""
    out = []
    for shape in shapes:
        label, expr = shape[0], shape[1]
        doc = shape[2] if len(shape) > 2 and shape[2] is not None else DOC
        where = shape[3] if len(shape) > 3 else "$project"
        ours, theirs = _answer(rust_mongo, expr, doc, where), _answer(mongod, expr, doc, where)
        if ours != theirs:
            out.append(f"{label}: mongod {theirs}, rust {ours}")
    return out


def _sort_orders(rust_mongo: Any, mongod: Any, values: list) -> list[str]:
    """`find().sort()` over `values`, without an index and then through one:
    the in-memory comparator and the stored index keys must both give
    mongod's order."""
    diffs = []
    for indexed in (False, True):
        answers = []
        for client in (rust_mongo, mongod):
            coll = _coll(client, "sorted")
            coll.drop()
            coll.insert_many([{"_id": i, "v": v} for i, v in enumerate(values)])
            cursor = coll.find().sort("v", 1)
            if indexed:
                coll.create_index("v")
                cursor = coll.find().sort("v", 1).hint("v_1")
            answers.append([d["_id"] for d in cursor])
        if answers[0] != answers[1]:
            label = "indexed" if indexed else "unindexed"
            diffs.append(f"$sort {label}: mongod {answers[1]}, rust {answers[0]}")
    return diffs


def _bin(data: bytes, subtype: int = 0) -> Binary:
    return Binary(data, subtype)


# ------------------------------------------------- comparison and equality --


@pytest.mark.backlog_fixed("RM-232")
def test_bindata_orders_by_length_then_subtype(rust_mongo: Any, mongod: Any) -> None:
    """mongod orders BinData by length, then subtype, then bytes. The Rust
    comparison orders by the bytes alone, which also moves `$match` and
    `$group` answers, not only expressions."""
    short, long_ = _bin(b"\x02"), _bin(b"\x01\x01")
    shapes = [
        ("$cmp length", {"$cmp": [short, long_]}),
        ("$lt subtype", {"$lt": [_bin(b"\x01", 0), _bin(b"\x01", 5)]}),
        ("$setUnion subtypes", {"$setUnion": [[_bin(b"\x01", 0)], [_bin(b"\x01", 5)]]}),
        ("$max", {"$max": [short, long_]}),
    ]
    diffs = _diverging(rust_mongo, mongod, shapes)
    for client in (rust_mongo, mongod):
        coll = _coll(client, "bins")
        coll.drop()
        coll.insert_many([{"_id": 1, "b": short}, {"_id": 2, "b": long_}])
    for name, run in (
        ("$match $gt", lambda c: [d["_id"] for d in c.find({"b": {"$gt": long_}})]),
        ("$group $max", lambda c: list(c.aggregate([{"$group": {"_id": 0, "m": {"$max": "$b"}}}]))),
    ):
        ours, theirs = run(_coll(rust_mongo, "bins")), run(_coll(mongod, "bins"))
        if ours != theirs:
            diffs.append(f"{name}: mongod {theirs}, rust {ours}")
    diffs += _sort_orders(rust_mongo, mongod, [_bin(b"\x01", 5), _bin(b"\x02"), _bin(b"\x01\x01")])
    assert not diffs, "\n".join(diffs)


@pytest.mark.backlog_fixed("RM-233")
def test_code_with_scope_is_its_own_type_and_scope_counts(rust_mongo: Any, mongod: Any) -> None:
    """mongod ranks JavaScript-with-scope apart from JavaScript and compares
    the scope; the Rust comparison gives them one rank and reads the code only."""
    shapes = [
        ("$cmp scope vs none", {"$cmp": [Code("a", {"x": 1}), Code("a")]}),
        ("$lt scopes", {"$lt": [Code("a", {"x": 1}), Code("a", {"x": 2})]}),
        ("$setEquals scopes", {"$setEquals": [[Code("a", {"x": 1})], [Code("a", {"x": 2})]]}),
    ]
    d = _diverging(rust_mongo, mongod, shapes)
    d += _sort_orders(
        rust_mongo,
        mongod,
        [Code("b", {"x": 1}), Code("c"), Code("a", {"x": 2}), Code("a", {"x": 1})],
    )
    assert not d, "\n".join(d)


@pytest.mark.backlog_fixed("RM-234")
def test_minkey_and_maxkey_equal_themselves(rust_mongo: Any, mongod: Any) -> None:
    """`{$eq: [MinKey, MinKey]}` is true on mongod; the Rust equality has no
    arm for either type and answers false, so `$ne`, `$in` and
    `$indexOfArray` invert too."""
    doc = {**DOC, "lo": MinKey(), "hi": MaxKey()}
    shapes = [
        ("$eq MinKey", {"$eq": [MinKey(), MinKey()]}, doc),
        ("$eq $hi", {"$eq": ["$hi", "$hi"]}, doc),
        ("$ne MaxKey", {"$ne": [MaxKey(), MaxKey()]}, doc),
        ("$in", {"$in": ["$lo", [1, MinKey()]]}, doc),
        ("$indexOfArray", {"$indexOfArray": [[MaxKey()], "$hi"]}, doc),
        ("nested", {"$eq": [{"a": MinKey()}, {"a": MinKey()}]}, doc),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-235")
def test_document_equality_respects_field_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod compares documents field by field in order, so `{b:1,a:1}` is
    not `{a:1,b:1}`; the Rust equality looks fields up by name."""
    doc = {**DOC, "o": {"a": 1, "b": 1}}
    shapes = [
        ("$eq literal", {"$eq": [{"$literal": {"b": 1, "a": 1}}, {"$literal": {"a": 1, "b": 1}}]}),
        ("$eq field", {"$eq": ["$o", {"$literal": {"b": 1, "a": 1}}]}, doc),
        ("$in", {"$in": [{"$literal": {"b": 1, "a": 1}}, [{"$literal": {"a": 1, "b": 1}}]]}),
        (
            "nested",
            {"$eq": [{"$literal": {"x": {"b": 1, "a": 1}}}, {"$literal": {"x": {"a": 1, "b": 1}}}]},
        ),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


# ------------------------------------------------------------------ regex --


@pytest.mark.backlog_fixed("RM-236")
def test_regex_classes_are_ascii(rust_mongo: Any, mongod: Any) -> None:
    """mongod's PCRE matches `\\w`, `\\d`, `\\s` and `\\b` against ASCII only,
    with or without the `u` flag; the Rust regex matches Unicode classes."""
    shapes = [
        ("\\w é", {"$regexMatch": {"input": "é", "regex": "^\\w$"}}),
        ("\\d arabic", {"$regexMatch": {"input": "١", "regex": "^\\d$"}}),
        ("\\s nbsp", {"$regexMatch": {"input": " ", "regex": "^\\s$"}}),
        ("\\b", {"$regexMatch": {"input": "éa", "regex": "\\ba"}}),
        ("$regexFind \\w+", {"$regexFind": {"input": "é1", "regex": "\\w+"}}),
        ("\\s VT", {"$regexMatch": {"input": "\x0b", "regex": "^\\s$"}}),
        ("\\B", {"$regexMatch": {"input": "é", "regex": "\\B"}}),
        ("[^\\w]", {"$regexMatch": {"input": "é", "regex": "^[^\\w]$"}}),
        ("i Kelvin \\w", {"$regexMatch": {"input": "\u212a", "regex": "^\\w$", "options": "i"}}),
        ("u flag", {"$regexMatch": {"input": "é", "regex": "^\\w$", "options": "u"}}),
    ]
    diffs = _diverging(rust_mongo, mongod, shapes)
    for client in (rust_mongo, mongod):
        coll = _coll(client, "re")
        coll.drop()
        coll.insert_many([{"_id": 1, "s": "é"}, {"_id": 2, "s": "e"}])
    ours = [d["_id"] for d in _coll(rust_mongo, "re").find({"s": {"$regex": "^\\w$"}})]
    theirs = [d["_id"] for d in _coll(mongod, "re").find({"s": {"$regex": "^\\w$"}})]
    if ours != theirs:
        diffs.append(f"find $regex: mongod {theirs}, rust {ours}")
    assert not diffs, "\n".join(diffs)


@pytest.mark.backlog_fixed("RM-237")
def test_regex_find_all_has_no_trailing_empty_match(rust_mongo: Any, mongod: Any) -> None:
    """An empty-matching pattern gives mongod one match per position before
    the end; the Rust server added one at the end of the string, and skipped
    an empty match straight after a non-empty one."""
    fa = lambda inp, rx: {"$regexFindAll": {"input": inp, "regex": rx}}  # noqa: E731
    shapes = [
        ("empty", {"$size": fa("abc", "")}),
        ("x*", {"$size": fa("abc", "x*")}),
        ("multibyte", {"$size": fa("é😀", "")}),
        ("empty input", fa("", "")),
        ("a* after a match", fa("abc", "a*")),
        ("c|", fa("abc", "c|")),
        ("b*", fa("ab", "b*")),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-238")
def test_regex_operator_options_are_validated(rust_mongo: Any, mongod: Any) -> None:
    """mongod rejects a bad flag (51108), flags given twice (51107), a NUL in
    the pattern or options (51109 / 51110) and a non-string option (51106);
    the Rust server matches anyway or answers 2 "not supported"."""
    shapes = []
    for op in ("$regexMatch", "$regexFind", "$regexFindAll"):
        shapes += [
            (f"{op} bad flag", {op: {"input": "abc", "regex": "b", "options": "z"}}),
            (f"{op} bad flag in regex", {op: {"input": "abc", "regex": Regex("b", "l")}}),
            (f"{op} flags twice", {op: {"input": "abc", "regex": Regex("b", "i"), "options": "i"}}),
            (f"{op} NUL pattern", {op: {"input": "abc", "regex": "b\x00"}}),
            (f"{op} NUL options", {op: {"input": "abc", "regex": "b", "options": "i\x00"}}),
            (f"{op} int options", {op: {"input": "abc", "regex": "b", "options": 1}}),
        ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


# ----------------------------------------------------- array operators --


@pytest.mark.backlog_fixed("RM-239")
def test_filter_limit(rust_mongo: Any, mongod: Any) -> None:
    """mongod takes an integral double `limit` and rejects zero, negatives,
    booleans, strings, NaN and fractions (327391 / 327392); the Rust server
    ignores a double limit and returns one element for the rest."""
    f = lambda lim: {"$filter": {"input": [3, 1, 2], "cond": True, "limit": lim}}  # noqa: E731
    shapes = [
        ("2.0", f(2.0)),
        ("Decimal 2", f(Decimal128("2"))),
        ("0", f(0)),
        ("-1", f(-1)),
        ("true", f(True)),
        ("string", f("x")),
        ("NaN", f(float("nan"))),
        ("1.5", f(1.5)),
        ("-0.0", f(-0.0)),
        ("long past int32", f(Int64(2**40))),
        ("null", f(None)),
        ("0 over an empty input", {"$filter": {"input": [], "cond": True, "limit": 0}}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-240")
def test_zip_defaults_and_options(rust_mongo: Any, mongod: Any) -> None:
    """mongod evaluates each `defaults` entry and validates the options
    (34462 / 34463 / 34466 / 34467); the Rust server returns the defaults
    unevaluated and accepts every malformed option."""
    z = lambda **kw: {"$zip": {"inputs": [[1], [1, 2]], **kw}}  # noqa: E731
    shapes = [
        ("defaults evaluated", z(useLongestLength=True, defaults=["$n", {"$add": [1, 1]}])),
        ("defaults without longest", z(defaults=[0, 0])),
        ("defaults wrong length", z(useLongestLength=True, defaults=[0])),
        ("defaults null", z(useLongestLength=True, defaults=None)),
        ("defaults int", z(useLongestLength=True, defaults=1)),
        ("longest 1", z(useLongestLength=1)),
        ("longest string", z(useLongestLength="yes")),
        ("longest null", z(useLongestLength=None)),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-241")
def test_sort_array(rust_mongo: Any, mongod: Any) -> None:
    """`$sortArray`: a scalar sorted by a field spec is MISSING for the sort
    on mongod, a `-1.0` direction is descending, and an invalid `sortBy` or
    input is an error (2942504-2942507). The Rust server sorts the scalar by
    its value, reads `-1.0` as ascending and sorts or answers 2 for the rest."""
    s = lambda inp, by: {"$sortArray": {"input": inp, "sortBy": by}}  # noqa: E731
    mixed = [3, 1, {"a": 1}, {"a": 0}]
    shapes = [
        ("scalars by field", s(mixed, {"a": 1})),
        ("-1.0", s([{"a": 1}, {"a": 2}], {"a": -1.0})),
        ("whole value -1.0", s([1, 3, 2], -1.0)),
        ("whole value Decimal", s([1, 3, 2], Decimal128("1"))),
        ("sortBy 0", s([1, 2], 0)),
        ("sortBy 2", s([1, 2], 2)),
        ("sortBy {a:0}", s([1, 2], {"a": 0})),
        ("sortBy {}", s([1, 2], {})),
        ("sortBy string", s([1, 2], "x")),
        ("input not array", s(5, 1)),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


# ---------------------------------------------------------------- $switch --


@pytest.mark.backlog_fixed("RM-242")
def test_switch(rust_mongo: Any, mongod: Any) -> None:
    """A `$switch` with no matching branch and no default is 40066 per
    document and 40069 when every case is a constant false (mongod refuses it
    while optimizing, even over an empty collection). The Rust server answers
    40066 for both in `$project`, and 2 "not supported" in a `find` filter or
    projection and in `$match`; a malformed `$switch` (40061-40065) is also 2."""
    nomatch = {"$switch": {"branches": [{"case": {"$gt": ["$n", 5]}, "then": 1}]}}
    allfalse = {"$switch": {"branches": [{"case": False, "then": 1}]}}
    sw = lambda **kw: {"$switch": kw}  # noqa: E731
    shapes = [
        ("all false", allfalse),
        ("null case", {"$switch": {"branches": [{"case": None, "then": 1}]}}),
        ("no match in $match", nomatch, None, "$match"),
        ("no match in find", nomatch, None, "find"),
        ("all false in find", allfalse, None, "find"),
        ("branches not array", sw(branches=1)),
        ("branches field path", sw(branches="$arr")),
        ("branch not document", sw(branches=[1], default=3)),
        ("branch without then", sw(branches=[{"case": True}], default=3)),
        ("branch without case", sw(branches=[{"then": 1}], default=3)),
        ("branch unknown key", sw(branches=[{"case": True, "then": 1, "x": 2}])),
    ]
    diffs = _diverging(rust_mongo, mongod, shapes)
    for client in (rust_mongo, mongod):
        _coll(client, "empty").drop()
    ours = mongo_answer(
        lambda: list(_coll(rust_mongo, "empty").aggregate([{"$project": {"z": allfalse}}]))
    )
    theirs = mongo_answer(
        lambda: list(_coll(mongod, "empty").aggregate([{"$project": {"z": allfalse}}]))
    )
    if ours[:2] != theirs[:2]:
        diffs.append(f"all false over no documents: mongod {theirs[:2]}, rust {ours[:2]}")
    assert not diffs, "\n".join(diffs)


# ------------------------------------------------------------- arithmetic --


@pytest.mark.backlog_fixed("RM-243")
def test_round_and_trunc_of_doubles(rust_mongo: Any, mongod: Any) -> None:
    """mongod rounds a double's exact binary value; the Rust server computes
    `(x * 10^p).round() / 10^p`, which rounds the decimal text, loses
    precision at large magnitudes and overflows to infinity."""
    shapes = [
        ("$trunc 1.45 2", {"$trunc": [1.45, 2]}),
        ("$trunc 0.045 3", {"$trunc": [0.045, 3]}),
        ("$round 2.675 2", {"$round": [2.675, 2]}),
        ("$round 1e300 10", {"$round": [1e300, 10]}),
        ("$round max 1", {"$round": [1.7976931348623157e308, 1]}),
        ("$round 1e20 3", {"$round": [1e20, 3]}),
        ("$round 1e15+0.5 1", {"$round": [1e15 + 0.5, 1]}),
        ("$trunc 1e23 -20", {"$trunc": [1e23, -20]}),
        ("$round int32 min -1", {"$round": [-2147483648, -1]}),
        ("$round NaN place", {"$round": [1.5, Decimal128("NaN")]}),
        ("$round null 200", {"$round": [None, 200]}),
        ("$round int32 max -1", {"$round": [2147483647, -1]}),
        ("$round string 200", {"$round": ["x", 200]}),
        ("$trunc string", {"$trunc": ["x", 0]}),
        ("$round 0.125 2", {"$round": [0.125, 2]}),
        ("$round -0.4 0", {"$round": [-0.4, 0]}),
        ("$trunc 123456789.123456789 5", {"$trunc": [123456789.123456789, 5]}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-244")
def test_pow_of_one_with_negative_exponent_stays_integral(rust_mongo: Any, mongod: Any) -> None:
    """`{$pow: [1, -5]}` is int32 1 on mongod (and -1 to an odd power is -1);
    the Rust server falls through to `powf` and answers the double."""
    shapes = [
        ("1 ^ -5", {"$pow": [1, -5]}),
        ("-1 ^ -5", {"$pow": [-1, -5]}),
        ("-1 ^ -4", {"$pow": [-1, -4]}),
        ("long 1 ^ -3", {"$pow": [Int64(1), -3]}),
        ("-1 ^ long -3", {"$pow": [-1, Int64(-3)]}),
        ("2 ^ -1", {"$pow": [2, -1]}),
        ("1 ^ long min", {"$pow": [1, Int64(-(2**63))]}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog("RM-245")
def test_decimal_hyperbolic_and_trig_edges(rust_mongo: Any, mongod: Any) -> None:
    """Decimal128 transcendentals past the authorised last digit: tiny
    operands underflow to 0 in `$sinh` / `$tanh` / `$atanh`, a zero keeps
    mongod's exponent, huge `$sin` arguments are 50989 instead of a value,
    and `$sinh` of 1E+34 is 2 "not supported" instead of Infinity."""
    d = Decimal128
    shapes = [
        ("$sinh 1E-100", {"$sinh": d("1E-100")}),
        ("$tanh 1E-100", {"$tanh": d("1E-100")}),
        ("$atanh 1E-100", {"$atanh": d("1E-100")}),
        ("$sin 1E-100", {"$sin": d("1E-100")}),
        ("$cos 1E-100", {"$cos": d("1E-100")}),
        ("$sin 0E+5", {"$sin": d("0E+5")}),
        ("$tan 0E+5", {"$tan": d("0E+5")}),
        ("$acos 1", {"$acos": d("1")}),
        ("$sin 1E+1200", {"$sin": d("1E+1200")}),
        ("$sinh 1E+34", {"$sinh": d("1E+34")}),
        ("$tanh 1E+34", {"$tanh": d("1E+34")}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


# ------------------------------------------------------------- conversion --


@pytest.mark.backlog("RM-246")
def test_convert(rust_mongo: Any, mongod: Any) -> None:
    """`$convert`: a double just past the int32 range is 241 on mongod, not a
    clamp, and a string below the smallest normal double is 241, not 0 or a
    subnormal."""
    c = lambda inp, to, **kw: {"$convert": {"input": inp, "to": to, **kw}}  # noqa: E731
    shapes = [
        ("2147483647.5 to int", c(2147483647.5, "int")),
        ("-2147483648.9 to int", c(-2147483648.9, "int")),
        ("1e-400 to double", c("1e-400", "double")),
        ("4.9e-324 to double", c("4.9e-324", "double")),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-246")
def test_convert_targets_and_fallbacks(rust_mongo: Any, mongod: Any) -> None:
    """`to` as a number, a non-integral, null or a type `$convert` cannot
    reach is a value or mongod's own error, not 2 "not supported"; an
    `onError` that evaluates to missing leaves the field missing; and the
    target is checked before a null input's `onNull`."""
    c = lambda inp, to, **kw: {"$convert": {"input": inp, "to": to, **kw}}  # noqa: E731
    shapes = [
        ("to 1.0", c(5, 1.0)),
        ("to Decimal 16", c(5, Decimal128("16"))),
        ("to 99", c(5, 99)),
        ("to 16.5", c(5, 16.5)),
        ("to null", c(5, None)),
        ("to regex", c(5, "regex")),
        ("to 4", c(5, 4)),
        ("to missing", {"$convert": {"input": 5}}),
        ("onError missing", c("x", "int", onError="$nope")),
        ("onNull with bogus to", c(None, "bogus", onNull=1)),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


# ------------------------------------------------------------------ dates --


@pytest.mark.backlog_fixed("RM-247")
def test_date_to_string_arguments(rust_mongo: Any, mongod: Any) -> None:
    """`$dateToString` evaluates `format` and honours `onNull`; the Rust
    server reads `format` as literal text, ignores `onNull`, refuses years
    1-999 with 2 and reports a trailing `%` as 18536 instead of 18535."""
    doc = {**DOC, "f": "%Y"}
    dts = lambda **kw: {"$dateToString": kw}  # noqa: E731
    shapes = [
        ("format field", dts(date="$t", format="$f"), doc),
        ("format missing field", dts(date="$t", format="$nope"), doc),
        ("format expression", dts(date="$t", format={"$concat": ["%Y", "-%m"]}), doc),
        ("format null", dts(date="$t", format=None), doc),
        ("onNull", dts(date=None, onNull="N")),
        ("onNull missing date", dts(date="$nope", onNull="N")),
        ("onNull missing", dts(date=None, onNull="$nope")),
        ("year 999", dts(date=datetime.datetime(999, 12, 31))),
        ("trailing %", dts(date="$t", format="abc%")),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-248")
def test_local_times_at_daylight_saving_changes(rust_mongo: Any, mongod: Any) -> None:
    """A local time that happens twice (the autumn change) is the LATER,
    standard-time instant on mongod -- except in `$dateAdd`, which keeps the
    start date's daylight saving; one that never happens (the spring change)
    uses the offset before the change. The Rust server took the earlier
    instant, and refused a gap time with 40485 "unrecognized time zone
    identifier"."""
    add = lambda start, unit, n=1, tz="America/New_York": {  # noqa: E731
        "$dateAdd": {"startDate": start, "unit": unit, "amount": n, "timezone": tz}
    }
    fp = lambda y, mo, d, h, mi, tz: {  # noqa: E731
        "$dateFromParts": {
            "year": y,
            "month": mo,
            "day": d,
            "hour": h,
            "minute": mi,
            "timezone": tz,
        }
    }
    shapes = [
        ("NY overlap", fp(2026, 11, 1, 1, 30, "America/New_York")),
        ("London overlap", fp(2026, 10, 25, 1, 0, "Europe/London")),
        ("Sydney overlap", fp(2026, 4, 5, 2, 30, "Australia/Sydney")),
        ("NY gap", fp(2026, 3, 8, 2, 30, "America/New_York")),
        ("London gap", fp(2026, 3, 29, 1, 30, "Europe/London")),
        ("Lord Howe half-hour gap", fp(2026, 10, 4, 2, 15, "Australia/Lord_Howe")),
        ("São Paulo midnight gap", fp(2018, 11, 4, 0, 30, "America/Sao_Paulo")),
        ("$dateAdd EDT day into overlap", add(datetime.datetime(2026, 10, 31, 5, 30), "day")),
        ("$dateAdd EST month into overlap", add(datetime.datetime(2026, 1, 1, 6, 30), "month", 10)),
        (
            "$dateAdd BST day into overlap",
            add(datetime.datetime(2026, 10, 24, 0, 0), "day", tz="Europe/London"),
        ),
        (
            "$dateAdd into gap",
            {
                "$dateAdd": {
                    "startDate": datetime.datetime(2026, 3, 7, 7, 30),
                    "unit": "day",
                    "amount": 1,
                    "timezone": "America/New_York",
                }
            },
        ),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-249")
def test_time_zone_rules_far_from_now(rust_mongo: Any, mongod: Any) -> None:
    """mongod answers from timelib's built-in timezonedb (2024.2: tzdata
    2024b with `backzone`): daylight saving after 2099, each zone's own
    history (Amsterdam 1900 is +0019), 2024b's rules today (Asuncion), and
    its own names. The Rust server's chrono-tz stopped daylight saving in
    2100, folded historical zones into their links, carried 2025b and knew a
    different set of names. `tools/probes/time_zones.py` sweeps every name."""
    fmt = lambda when, tz: {  # noqa: E731
        "$dateToString": {"date": when, "format": "%H:%M:%S %z", "timezone": tz}
    }
    shapes = [
        ("NY 2100", fmt(datetime.datetime(2100, 7, 1, 12), "America/New_York")),
        ("London 2100", fmt(datetime.datetime(2100, 7, 1, 12), "Europe/London")),
        ("Amsterdam 1900", fmt(datetime.datetime(1900, 7, 1, 12), "Europe/Amsterdam")),
        ("Stockholm 1945", fmt(datetime.datetime(1945, 7, 1, 12), "Europe/Stockholm")),
        ("Reykjavik 1930", fmt(datetime.datetime(1930, 7, 1, 12), "Atlantic/Reykjavik")),
        ("NY 9000", fmt(datetime.datetime(9000, 7, 1, 12), "America/New_York")),
        ("Sydney 2150", fmt(datetime.datetime(2150, 1, 1, 12), "Australia/Sydney")),
        ("Asuncion 2025", fmt(datetime.datetime(2025, 7, 1, 12), "America/Asuncion")),
        ("Accra 1850 %z", fmt(datetime.datetime(1850, 7, 1, 12), "Africa/Accra")),
        ("Factory", fmt(datetime.datetime(2020, 1, 1), "Factory")),
        ("Coyhaique", fmt(datetime.datetime(2025, 7, 1), "America/Coyhaique")),
        ("lower-case name", fmt(datetime.datetime(2020, 1, 1), "america/new_york")),
        (
            "fromParts NY 2150 gap",
            {
                "$dateFromParts": {
                    "year": 2150,
                    "month": 3,
                    "day": 8,
                    "hour": 2,
                    "minute": 30,
                    "timezone": "America/New_York",
                }
            },
        ),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-250")
def test_date_arithmetic_arguments_and_range(rust_mongo: Any, mongod: Any) -> None:
    """`$dateAdd` / `$dateTrunc` / `$dateDiff` / `$dateToParts`: mongod
    accepts any int64 millisecond result, an ObjectId or Timestamp date, an
    integral Decimal128 amount or binSize, answers null for a null unit and
    rejects a huge binSize (5439006). The Rust server answers 2 "not
    supported" for most of these and silently returns 2000-01-01 for the
    huge binSize."""
    t = "$t"
    add = lambda **kw: {"$dateAdd": {"startDate": t, **kw}}  # noqa: E731
    trunc = lambda **kw: {"$dateTrunc": {"date": t, **kw}}  # noqa: E731
    shapes = [
        ("$dateAdd 8000 years", add(unit="year", amount=8000)),
        ("$dateAdd 1e15 ms", add(unit="millisecond", amount=Int64(10**15))),
        ("$dateAdd unit null", add(unit=None, amount=1)),
        ("$dateAdd Decimal amount", add(unit="day", amount=Decimal128("2"))),
        ("$dateAdd fractional amount", add(unit="day", amount=1.5)),
        ("$dateAdd string start", {"$dateAdd": {"startDate": "x", "unit": "day", "amount": 1}}),
        ("$dateTrunc huge binSize", trunc(unit="day", binSize=10**12)),
        ("$dateTrunc binSize 0", trunc(unit="day", binSize=0)),
        ("$dateTrunc Decimal binSize", trunc(unit="day", binSize=Decimal128("2"))),
        ("$dateTrunc unit null", trunc(unit=None)),
        ("$dateToParts iso8601 int", {"$dateToParts": {"date": t, "iso8601": 1}}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


# ------------------------------------------------- the remaining shapes --


@pytest.mark.backlog("RM-251")
def test_constant_arguments_are_checked_while_optimizing(rust_mongo: Any, mongod: Any) -> None:
    """mongod evaluates every constant sub-expression when it optimizes the
    pipeline, so an erroring constant fails even in a branch that is never
    taken, and a constant `$dateToString` format is validated even when the
    date is null. The Rust server evaluates lazily and answers."""
    shapes = [
        ("$cond untaken", {"$cond": [True, 1, {"$divide": [1, 0]}]}),
        ("$ifNull untaken", {"$ifNull": [1, {"$divide": [1, 0]}]}),
        ("$and short circuit", {"$and": [False, {"$divide": [1, 0]}]}),
        ("$map over empty", {"$map": {"input": [], "in": {"$divide": [1, 0]}}}),
        ("$dateToString bad format null date", {"$dateToString": {"date": None, "format": "%Q"}}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog("RM-252")
def test_operator_tail_codes(rust_mongo: Any, mongod: Any) -> None:
    """One shape each, a wrong code, a refusal or a null for a missing value:
    `$arrayElemAt` of a null array with a bad index, `$percentile` with a
    Decimal128 `p`, `$meta: "sortKey"` in `$match`, `$getField` of
    `$$REMOVE`, and `$setField` with a field path for `field`."""
    shapes = [
        ("$arrayElemAt null array string index", {"$arrayElemAt": [None, "x"]}),
        (
            "$percentile Decimal p",
            {"$percentile": {"input": [1, 2], "p": [Decimal128("0.5")], "method": "approximate"}},
        ),
        ("$meta sortKey in $match", {"$meta": "sortKey"}, None, "$match"),
        ("$getField of $$REMOVE", {"$getField": {"field": "x", "input": "$$REMOVE"}}),
        ("$setField field path", {"$setField": {"field": "$s", "input": {}, "value": 1}}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog_fixed("RM-252")
def test_operator_tail_codes_fixed(rust_mongo: Any, mongod: Any) -> None:
    """The `$firstN` family with a NaN or Decimal128 `n`, `$toString` of year
    1 and `$meta: "randVal"` answer as mongod does."""
    shapes = [
        ("$firstN NaN n", {"$firstN": {"input": [1, 2], "n": float("nan")}}),
        ("$firstN Decimal n", {"$firstN": {"input": [1, 2], "n": Decimal128("1")}}),
        ("$toString year 1", {"$toString": datetime.datetime(1, 1, 1)}),
        ("$meta randVal", {"$meta": "randVal"}),
    ]
    assert not (d := _diverging(rust_mongo, mongod, shapes)), "\n".join(d)


@pytest.mark.backlog("RM-253")
def test_bucket_with_non_numeric_boundaries(rust_mongo: Any, mongod: Any) -> None:
    """`$bucket` emits buckets in `_id` order, so a string `default` comes
    before date buckets; the Rust server puts the default last, and answers 2
    "not supported" for Decimal128 or mixed numeric boundaries."""
    docs = [{"_id": i, "v": v} for i, v in enumerate([datetime.datetime(2020, 1, 1), "x", 5])]

    def run(client: Any, spec: dict) -> Any:
        coll = _coll(client, "bucket")
        coll.drop()
        coll.insert_many(docs)
        return mongo_answer(lambda: list(coll.aggregate([{"$bucket": spec}])))[:2]

    specs = {
        "date boundaries, string default": {
            "groupBy": "$v",
            "boundaries": [datetime.datetime(2019, 1, 1), datetime.datetime(2021, 1, 1)],
            "default": "other",
        },
        "decimal boundaries": {
            "groupBy": "$v",
            "boundaries": [Decimal128("0"), Decimal128("10")],
            "default": "o",
        },
        "mixed numeric": {
            "groupBy": "$v",
            "boundaries": [0, Decimal128("10"), 20.5],
            "default": "o",
        },
    }
    diffs = []
    for name, spec in specs.items():
        ours, theirs = run(rust_mongo, spec), run(mongod, spec)
        if ours != theirs:
            diffs.append(f"{name}: mongod {theirs}, rust {ours}")
    assert not diffs, "\n".join(diffs)
