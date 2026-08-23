from __future__ import annotations

import math

import pytest

from secantus.expressions import MISSING, ExpressionError, evaluate


def test_literal_passthrough() -> None:
    assert evaluate(5, {}) == 5
    assert evaluate("hello", {}) == "hello"
    assert evaluate(None, {}) is None
    assert evaluate(True, {}) is True


def test_field_path() -> None:
    doc = {"a": 1, "nested": {"b": 2}}
    assert evaluate("$a", doc) == 1
    assert evaluate("$nested.b", doc) == 2
    assert evaluate("$missing", doc) is None


def test_literal_op_blocks_recursion() -> None:
    assert evaluate({"$literal": "$a"}, {"a": 1}) == "$a"


def test_arithmetic() -> None:
    doc = {"x": 3, "y": 4}
    assert evaluate({"$add": ["$x", "$y", 10]}, doc) == 17
    assert evaluate({"$subtract": ["$y", "$x"]}, doc) == 1
    assert evaluate({"$multiply": ["$x", "$y"]}, doc) == 12
    assert evaluate({"$divide": ["$y", "$x"]}, doc) == pytest.approx(4 / 3)
    assert evaluate({"$mod": [10, 3]}, doc) == 1


def test_arithmetic_with_null_returns_null() -> None:
    assert evaluate({"$add": ["$missing", 5]}, {}) is None
    # Null propagates BEFORE type checks, matching mongod.
    assert evaluate({"$multiply": [None, "x"]}, {}) is None


def test_arithmetic_rejects_non_numeric() -> None:
    """mongod raises on non-numeric arithmetic operands; message and code
    shapes verified against mongod 8.2 (2026-06-12 oracle probe)."""
    with pytest.raises(
        ExpressionError, match=r"\$multiply only supports numeric types, not string"
    ):
        evaluate({"$multiply": [2, "x"]}, {})
    with pytest.raises(ExpressionError, match=r"\$multiply only supports numeric types, not bool"):
        evaluate({"$multiply": [2, True]}, {})
    with pytest.raises(ExpressionError, match=r"\$add only supports numeric or date types"):
        evaluate({"$add": [2, "x"]}, {})
    # Single-arg forms type-check too.
    with pytest.raises(ExpressionError, match=r"\$add only supports"):
        evaluate({"$add": ["x"]}, {})
    with pytest.raises(ExpressionError, match=r"can't \$subtract string from int"):
        evaluate({"$subtract": [2, "x"]}, {})
    with pytest.raises(
        ExpressionError, match=r"\$divide only supports numeric types, not int and string"
    ):
        evaluate({"$divide": [2, "x"]}, {})
    with pytest.raises(ExpressionError, match=r"\$mod only supports numeric types") as exc_info:
        evaluate({"$mod": [2, "x"]}, {})
    assert exc_info.value.code == 16611


def test_divide_and_mod_by_zero_raise() -> None:
    with pytest.raises(ExpressionError, match=r"can't \$divide by zero") as div_exc:
        evaluate({"$divide": [5, 0]}, {})
    assert div_exc.value.code == 2
    assert div_exc.value.code_name == "BadValue"
    with pytest.raises(ExpressionError, match=r"can't \$mod by zero") as mod_exc:
        evaluate({"$mod": [5, 0]}, {})
    assert mod_exc.value.code == 16610


def test_arithmetic_date_semantics() -> None:
    import datetime as dt

    from bson import Int64

    d = dt.datetime(2020, 1, 1)
    assert evaluate({"$add": ["$d", 1000]}, {"d": d}) == d + dt.timedelta(seconds=1)
    assert evaluate({"$subtract": ["$d", 1000]}, {"d": d}) == d - dt.timedelta(seconds=1)
    diff = evaluate({"$subtract": ["$d2", "$d"]}, {"d": d, "d2": d + dt.timedelta(seconds=2)})
    assert diff == Int64(2000)
    with pytest.raises(ExpressionError, match=r"only one date allowed in an \$add"):
        evaluate({"$add": ["$d", "$d"]}, {"d": d})
    with pytest.raises(ExpressionError, match=r"can't \$subtract date from int"):
        evaluate({"$subtract": [1000, "$d"]}, {"d": d})


def test_concat() -> None:
    assert evaluate({"$concat": ["hello", " ", "world"]}, {}) == "hello world"
    assert evaluate({"$concat": ["a", "$s"]}, {"s": "b"}) == "ab"


def test_concat_type_validation() -> None:
    # mongod: a non-string operand is Location16702 (no str() coercion); a null /
    # missing operand short-circuits to a null result, left-to-right.
    for bad in ([("x=", 5)], [("x=", True)], [(5,)], [("a", ["b"])]):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$concat": list(bad[0])}, {})
        assert exc.value.code == 16702, bad
    assert evaluate({"$concat": ["a", None, "b"]}, {}) is None
    assert evaluate({"$concat": ["a", "$missing", "b"]}, {}) is None
    # Left-to-right: a non-string before a null still raises.
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$concat": [5, None]}, {})
    assert exc.value.code == 16702
    # A null before a non-string short-circuits to null.
    assert evaluate({"$concat": [None, 5]}, {}) is None


def test_comparisons() -> None:
    doc = {"a": 5}
    assert evaluate({"$eq": ["$a", 5]}, doc) is True
    assert evaluate({"$ne": ["$a", 5]}, doc) is False
    assert evaluate({"$gt": ["$a", 4]}, doc) is True
    assert evaluate({"$lt": ["$a", 4]}, doc) is False


def test_logical() -> None:
    assert evaluate({"$and": [True, True]}, {}) is True
    assert evaluate({"$and": [True, False]}, {}) is False
    assert evaluate({"$or": [False, True]}, {}) is True
    assert evaluate({"$not": [False]}, {}) is True


def test_cond_dict_form() -> None:
    doc = {"x": 5}
    expr = {"$cond": {"if": {"$gt": ["$x", 3]}, "then": "big", "else": "small"}}
    assert evaluate(expr, doc) == "big"


def test_cond_array_form() -> None:
    doc = {"x": 1}
    expr = {"$cond": [{"$gt": ["$x", 3]}, "big", "small"]}
    assert evaluate(expr, doc) == "small"


def test_if_null() -> None:
    assert evaluate({"$ifNull": ["$missing", "default"]}, {}) == "default"
    assert evaluate({"$ifNull": ["$present", "default"]}, {"present": 1}) == 1


def test_size() -> None:
    assert evaluate({"$size": "$tags"}, {"tags": [1, 2, 3]}) == 3


def test_string_case() -> None:
    assert evaluate({"$toUpper": "$s"}, {"s": "hello"}) == "HELLO"
    assert evaluate({"$toLower": "$s"}, {"s": "HELLO"}) == "hello"


def test_unsupported_operator_raises() -> None:
    with pytest.raises(ExpressionError):
        evaluate({"$bogus": 1}, {})


def test_dict_with_no_dollar_key_evaluates_each_field() -> None:
    out = evaluate({"a": "$x", "b": {"$add": ["$x", 1]}}, {"x": 5})
    assert out == {"a": 5, "b": 6}


def test_date_extractors() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 27, 13, 14, 15)
    doc = {"ts": when}
    assert evaluate({"$year": "$ts"}, doc) == 2026
    assert evaluate({"$month": "$ts"}, doc) == 4
    assert evaluate({"$dayOfMonth": "$ts"}, doc) == 27
    assert evaluate({"$hour": "$ts"}, doc) == 13
    assert evaluate({"$minute": "$ts"}, doc) == 14
    assert evaluate({"$second": "$ts"}, doc) == 15


def test_date_to_string_default() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 27, 13, 14, 15, 678000)
    out = evaluate({"$dateToString": {"date": "$ts"}}, {"ts": when})
    assert out == "2026-04-27T13:14:15.678Z"


def test_date_to_string_custom_format() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 27)
    out = evaluate({"$dateToString": {"date": "$ts", "format": "%Y/%m/%d"}}, {"ts": when})
    assert out == "2026/04/27"


def test_date_to_string_iso_year_and_week() -> None:
    """``%G`` returns the ISO 8601 year, ``%V`` the ISO week 01-53.

    These differ from ``%Y`` / ``%U`` near the year boundary: a
    January date that belongs to the prior ISO year shows the prior
    year under ``%G``."""
    import datetime as dt

    # 2026-01-01 is a Thursday — ISO week 1 of 2026.
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%G-W%V"}},
        {"ts": dt.datetime(2026, 1, 1)},
    )
    assert out == "2026-W01"

    # 2027-01-01 is a Friday — that's still ISO week 53 of 2026, not 2027's week 1.
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%G-W%V"}},
        {"ts": dt.datetime(2027, 1, 1)},
    )
    assert out == "2026-W53"


def test_date_to_string_day_of_year() -> None:
    """``%j`` returns the 3-digit day of year (001-366)."""
    import datetime as dt

    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%j"}},
        {"ts": dt.datetime(2026, 1, 1)},
    )
    assert out == "001"
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%j"}},
        {"ts": dt.datetime(2026, 12, 31)},
    )
    assert out == "365"  # 2026 is a non-leap year


def test_date_to_string_weekday_mongod_numbering() -> None:
    """``%w`` uses mongod's 1-Sunday … 7-Saturday numbering, not
    Python's 0-Sunday … 6-Saturday. The handler does the conversion
    so format strings written against mongod's docs work as-is."""
    import datetime as dt

    cases = [
        (dt.datetime(2026, 1, 4), "1"),  # Sunday
        (dt.datetime(2026, 1, 5), "2"),  # Monday
        (dt.datetime(2026, 1, 6), "3"),  # Tuesday
        (dt.datetime(2026, 1, 7), "4"),  # Wednesday
        (dt.datetime(2026, 1, 8), "5"),  # Thursday
        (dt.datetime(2026, 1, 9), "6"),  # Friday
        (dt.datetime(2026, 1, 10), "7"),  # Saturday
    ]
    for when, expected in cases:
        out = evaluate({"$dateToString": {"date": "$ts", "format": "%w"}}, {"ts": when})
        assert out == expected, f"{when.strftime('%A')}: got {out!r}, want {expected!r}"


def test_date_to_string_iso_weekday_passthrough() -> None:
    """``%u`` (ISO weekday 1-Mon … 7-Sun) is identical between mongod
    and Python's strftime — passes straight through."""
    import datetime as dt

    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%u"}},
        {"ts": dt.datetime(2026, 1, 5)},  # Monday
    )
    assert out == "1"
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%u"}},
        {"ts": dt.datetime(2026, 1, 11)},  # Sunday
    )
    assert out == "7"


def test_date_from_string_iso_year_and_week() -> None:
    """``$dateFromString`` accepts ``%G``/``%V`` (with ``%u`` to
    disambiguate the day) round-tripping with ``$dateToString``."""
    out = evaluate(
        {"$dateFromString": {"dateString": "2026-W01-1", "format": "%G-W%V-%u"}},
        {},
    )
    import datetime as dt

    assert isinstance(out, dt.datetime)
    # 2026-W01-1 = Monday Dec 29, 2025 (ISO week 1 of 2026 starts Mon Dec 29).
    assert out == dt.datetime(2025, 12, 29)


def test_array_elem_at() -> None:
    doc = {"a": [10, 20, 30]}
    assert evaluate({"$arrayElemAt": ["$a", 0]}, doc) == 10
    assert evaluate({"$arrayElemAt": ["$a", -1]}, doc) == 30
    # Out of range evaluates to MISSING, not null, so `$project` omits the field.
    # This used to assert `is None`, which added a field mongod does not send —
    # probed against mongod 6.0.16, where index 99 and index -99 on `[1, 2]` both
    # project `{_id: 1}` with no field at all.
    assert evaluate({"$arrayElemAt": ["$a", 99]}, doc) is MISSING
    assert evaluate({"$arrayElemAt": ["$a", -99]}, doc) is MISSING
    # A missing or null input array really is null, and is unchanged.
    assert evaluate({"$arrayElemAt": ["$nope", 0]}, doc) is None


def test_first_last_slice_reverse() -> None:
    doc = {"a": [1, 2, 3, 4]}
    assert evaluate({"$first": "$a"}, doc) == 1
    assert evaluate({"$last": "$a"}, doc) == 4
    assert evaluate({"$slice": ["$a", 2]}, doc) == [1, 2]
    assert evaluate({"$slice": ["$a", -2]}, doc) == [3, 4]
    assert evaluate({"$slice": ["$a", 1, 2]}, doc) == [2, 3]
    assert evaluate({"$reverseArray": "$a"}, doc) == [4, 3, 2, 1]


def test_concat_arrays() -> None:
    out = evaluate({"$concatArrays": [[1, 2], [3], [4, 5]]}, {})
    assert out == [1, 2, 3, 4, 5]


def test_array_operators_reject_non_array_input() -> None:
    # mongod codes for a non-array (non-null) input to each array operator.
    for expr, code in [
        ({"$first": 5}, 28689),
        ({"$last": "x"}, 28689),
        ({"$reverseArray": 5}, 34435),
        ({"$concatArrays": [[1], 5]}, 28664),
        ({"$slice": [5, 2]}, 28724),
        ({"$map": {"input": 5, "in": "$$this"}}, 16883),
        ({"$filter": {"input": 5, "cond": True}}, 28651),
        ({"$reduce": {"input": 5, "initialValue": 0, "in": "$$value"}}, 40080),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {})
        assert exc.value.code == code, expr
    # A null / missing input yields null (no error) for each.
    assert evaluate({"$first": None}, {}) is None
    assert evaluate({"$reverseArray": "$gone"}, {}) is None
    assert evaluate({"$map": {"input": None, "in": "$$this"}}, {}) is None
    assert evaluate({"$concatArrays": [[1], None]}, {}) is None


def test_in_operator_in_expressions() -> None:
    assert evaluate({"$in": ["b", ["a", "b", "c"]]}, {}) is True
    assert evaluate({"$in": ["x", ["a", "b", "c"]]}, {}) is False


def test_to_int_conversions() -> None:
    assert evaluate({"$toInt": 3.7}, {}) == 3
    assert evaluate({"$toInt": "42"}, {}) == 42
    assert evaluate({"$toInt": True}, {}) == 1


def test_to_int_overflow() -> None:
    from bson.int64 import Int64

    # int32 target: values outside [-2^31, 2^31-1] overflow (mongod 241).
    for bad in (3e9, 1e30, Int64(2**40), float("inf"), float("nan")):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$toInt": bad}, {})
        assert exc.value.code == 241
    # A long that fits int32 downcasts to a plain int (int32 on the wire).
    result = evaluate({"$toInt": Int64(5)}, {})
    assert result == 5 and not isinstance(result, Int64)
    # int32 boundaries are accepted.
    assert evaluate({"$toInt": 2147483647.0}, {}) == 2147483647
    assert evaluate({"$toInt": -2147483648.0}, {}) == -2147483648


def test_to_long_conversions() -> None:
    from bson.int64 import Int64

    # Truncates toward zero, parses strings, bool -> 0/1, and always yields Int64.
    for arg, want in ((3.7, 3), (-3.9, -3), ("42", 42), (True, 1), (5, 5)):
        result = evaluate({"$toLong": arg}, {})
        assert result == want and isinstance(result, Int64), arg
    # A value beyond int32 (but within int64) is fine for $toLong.
    result = evaluate({"$toLong": 9_000_000_000.0}, {})
    assert result == 9_000_000_000 and isinstance(result, Int64)
    # Int64 passes through.
    assert evaluate({"$toLong": Int64(2**40)}, {}) == 2**40
    # Missing / null -> null.
    assert evaluate({"$toLong": "$x"}, {}) is None


def test_to_long_overflow() -> None:
    # Beyond [-2^63, 2^63-1] or non-finite -> overflow (mongod 241).
    for bad in (1e30, float("inf"), float("nan"), "99999999999999999999"):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$toLong": bad}, {})
        assert exc.value.code == 241


def test_conversion_error_codes() -> None:
    # mongod-specific error codes (previously a generic TypeMismatch 14):
    # an unparseable numeric string -> ConversionFailure 241; an unknown
    # $convert target type -> 2 (uncatchable by onError); $sortArray non-array
    # -> 2942504; $strLenCP / $strLenBytes non-string -> 34471 / 34473.
    for op in ("$toInt", "$toLong", "$toDouble", "$toDecimal"):
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: "abc"}, {})
        assert exc.value.code == 241, op
    for expr, code in [
        ({"$convert": {"input": 5, "to": "bogus"}}, 2),
        ({"$convert": {"input": 5, "to": "bogus", "onError": -1}}, 2),  # not caught by onError
        ({"$sortArray": {"input": 5, "sortBy": 1}}, 2942504),
        ({"$strLenCP": 5}, 34471),
        ({"$strLenBytes": 5}, 34473),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {})
        assert exc.value.code == code, expr
    # Valid conversions and a caught bad-number onError still work.
    assert evaluate({"$toLong": "42"}, {}) == 42
    assert evaluate({"$convert": {"input": "abc", "to": "int", "onError": -1}}, {}) == -1


def test_array_set_typeguard_error_codes() -> None:
    # Array/set operators reject a non-array/non-object argument with mongod's
    # exact Location code (previously a silent accept for $arrayElemAt/$in, or a
    # generic TypeMismatch 14 for the rest).
    for expr, code in [
        ({"$size": 5}, 17124),
        ({"$arrayElemAt": [5, 0]}, 28689),
        ({"$in": [1, 5]}, 40081),
        ({"$indexOfArray": [5, 1]}, 40090),
        ({"$setUnion": [5]}, 17043),
        ({"$setIntersection": [5]}, 17047),
        ({"$setDifference": [5, 6]}, 17048),
        ({"$setIsSubset": [5, 6]}, 17046),
        ({"$anyElementTrue": 5}, 17041),
        ({"$allElementsTrue": 5}, 17040),
        ({"$mergeObjects": [5]}, 40400),
        ({"$range": ["a", "b"]}, 34443),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {})
        assert exc.value.code == code, expr
    # Valid forms still compute.
    assert evaluate({"$size": [1, 2, 3]}, {}) == 3
    assert evaluate({"$in": [2, [1, 2, 3]]}, {}) is True
    assert evaluate({"$arrayElemAt": [[10, 20], 1]}, {}) == 20
    assert evaluate({"$mergeObjects": [{"a": 1}, {"b": 2}]}, {}) == {"a": 1, "b": 2}


def test_string_typeguard_error_codes() -> None:
    # String/binary operators reject a non-string argument with mongod's exact
    # code. $regexMatch/$regexFind/$regexFindAll previously silently accepted a
    # non-string input (returning false/null/[]); a null input stays valid.
    for expr, code in [
        ({"$regexMatch": {"input": 5, "regex": "a"}}, 51104),
        ({"$regexFind": {"input": 5, "regex": "a"}}, 51104),
        ({"$regexFindAll": {"input": 5, "regex": "a"}}, 51104),
        ({"$indexOfBytes": [5, "a"]}, 40091),
        ({"$binarySize": 5}, 51276),
        ({"$bsonSize": 5}, 31393),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {})
        assert exc.value.code == code, expr
    # Null input is not an error for the regex operators.
    assert evaluate({"$regexMatch": {"input": None, "regex": "a"}}, {}) is False
    assert evaluate({"$regexMatch": {"input": "abc", "regex": "b"}}, {}) is True
    assert evaluate({"$binarySize": "abc"}, {}) == 3


def test_strcasecmp_coercion() -> None:
    # mongod $toString-coerces $strcasecmp operands (null -> ""), rejecting only
    # bool (Location16007) — SecantusDB previously rejected any non-string (14).
    assert evaluate({"$strcasecmp": [5, "a"]}, {}) == -1  # "5" < "a"
    assert evaluate({"$strcasecmp": ["a", 5]}, {}) == 1
    assert evaluate({"$strcasecmp": [5, 10]}, {}) == 1  # "5" > "10"
    assert evaluate({"$strcasecmp": [None, "a"]}, {}) == -1  # "" < "a"
    assert evaluate({"$strcasecmp": ["ABC", "abc"]}, {}) == 0  # case-insensitive
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$strcasecmp": [True, "a"]}, {})
    assert exc.value.code == 16007


def test_expression_accumulators() -> None:
    # MongoDB 5.0+ $sum/$avg/$max/$min as *expression* operators (not just group
    # accumulators): an array argument reduces over its elements, a scalar is a
    # single value, a missing/absent argument contributes nothing, non-numeric
    # elements are ignored by $sum/$avg, and $max/$min order by BSON cross-type
    # order ignoring null. Verified against mongod 7.0.12.
    assert evaluate({"$sum": "$arr"}, {"arr": [1, 2, 3]}) == 6
    assert evaluate({"$sum": "$n"}, {"n": 5}) == 5
    assert evaluate({"$sum": [1, 2, 3]}, {}) == 6
    assert evaluate({"$sum": ["$n", 10, "skip"]}, {"n": 5}) == 15  # non-numeric ignored
    assert evaluate({"$avg": "$arr"}, {"arr": [1, 2, 3]}) == 2.0
    assert evaluate({"$avg": "$n"}, {"n": 5}) == 5.0
    assert evaluate({"$max": "$arr"}, {"arr": [1, 2, 3]}) == 3
    assert evaluate({"$min": "$arr"}, {"arr": [1, 2, 3]}) == 1
    assert evaluate({"$max": "$n"}, {"n": 5}) == 5
    # Empty / missing edges: $sum -> 0, $avg/$max/$min -> null.
    assert evaluate({"$sum": []}, {}) == 0
    assert evaluate({"$avg": []}, {}) is None
    assert evaluate({"$max": []}, {}) is None
    assert evaluate({"$sum": "$missing"}, {}) == 0
    assert evaluate({"$max": "$missing"}, {}) is None


def test_date_misc_typeguard_error_codes() -> None:
    import datetime

    d = {"$literal": datetime.datetime(2020, 1, 1)}
    # Date/misc operators match mongod's error codes. $dateToString on a non-date
    # (and $dateDiff missing endDate) previously silently returned a value.
    for expr, code in [
        ({"$dateToString": {"date": "x"}}, 16006),
        ({"$dateToParts": {"date": "x"}}, 16006),
        ({"$dateFromString": {"dateString": 5}}, 241),
        ({"$dateAdd": {"startDate": d, "unit": "bogus", "amount": 1}}, 9),
        ({"$dateTrunc": {"date": d, "unit": "bogus"}}, 9),
        ({"$let": {"vars": {}, "in": "$$x"}}, 17276),
        ({"$switch": {"branches": []}}, 40068),
        ({"$ifNull": [1]}, 1257300),
        ({"$getField": {"field": 5, "input": {}}}, 5654602),
        ({"$setField": {"field": 5, "input": {}, "value": 1}}, 4161107),
        ({"$sortArray": {"input": [1], "sortBy": "x"}}, 2942507),
        ({"$convert": {"input": 5}}, 9),
        ({"$dateDiff": {"startDate": d}}, 5166304),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {"_id": 1})
        assert exc.value.code == code, expr
    # Valid forms still compute.
    assert evaluate({"$ifNull": [None, 7]}, {}) == 7
    assert evaluate({"$let": {"vars": {"a": 5}, "in": "$$a"}}, {}) == 5
    assert evaluate({"$switch": {"branches": [{"case": True, "then": 9}]}}, {}) == 9


def test_more_expression_error_codes() -> None:
    import datetime

    # More mongod-specific codes (previously generic 14): $zip non-array inputs /
    # element (34461/34468); $arrayToObject non-array (40386); $objectToArray
    # non-document (40390); $replaceOne/$replaceAll per-argument (51746/51745/
    # 51744); $dateDiff unknown unit (9).
    for expr, code in [
        ({"$zip": {"inputs": 5}}, 34461),
        ({"$zip": {"inputs": [5]}}, 34468),
        ({"$arrayToObject": 5}, 40386),
        ({"$objectToArray": 5}, 40390),
        ({"$replaceOne": {"input": 5, "find": "a", "replacement": "b"}}, 51746),
        ({"$replaceOne": {"input": "a", "find": 5, "replacement": "b"}}, 51745),
        ({"$replaceAll": {"input": "a", "find": "a", "replacement": 5}}, 51744),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {})
        assert exc.value.code == code, expr
    d1, d2 = datetime.datetime(2020, 1, 1), datetime.datetime(2021, 1, 1)
    with pytest.raises(ExpressionError) as exc:
        evaluate(
            {
                "$dateDiff": {
                    "startDate": {"$literal": d1},
                    "endDate": {"$literal": d2},
                    "unit": "bogus",
                }
            },
            {},
        )
    assert exc.value.code == 9
    # Valid forms still work.
    assert evaluate({"$zip": {"inputs": [[1, 2], [3, 4]]}}, {}) == [[1, 3], [2, 4]]
    assert evaluate({"$replaceOne": {"input": "aXa", "find": "X", "replacement": "-"}}, {}) == "a-a"


def test_convert_int_long_overflow() -> None:
    # $convert to int/long range-checks and raises 241 (caught by onError).
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$convert": {"input": 3e9, "to": "int"}}, {})
    assert exc.value.code == 241
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$convert": {"input": 9.3e18, "to": "long"}}, {})
    assert exc.value.code == 241
    assert evaluate({"$convert": {"input": 1e30, "to": "int", "onError": "oops"}}, {}) == "oops"
    # In-range values convert normally.
    assert evaluate({"$convert": {"input": 5.0, "to": "long"}}, {}) == 5


def test_to_double_conversions() -> None:
    assert evaluate({"$toDouble": 3}, {}) == 3.0
    assert evaluate({"$toDouble": "3.14"}, {}) == 3.14


def test_to_bool_conversions() -> None:
    assert evaluate({"$toBool": 0}, {}) is False
    assert evaluate({"$toBool": "x"}, {}) is True
    assert evaluate({"$toBool": ""}, {}) is False


def test_filter_basic() -> None:
    expr = {"$filter": {"input": "$nums", "as": "n", "cond": {"$gt": ["$$n", 2]}}}
    assert evaluate(expr, {"nums": [1, 2, 3, 4]}) == [3, 4]


def test_filter_with_default_var_name() -> None:
    expr = {"$filter": {"input": "$nums", "cond": {"$gte": ["$$this", 2]}}}
    assert evaluate(expr, {"nums": [1, 2, 3]}) == [2, 3]


def test_filter_with_limit() -> None:
    expr = {
        "$filter": {
            "input": "$nums",
            "as": "n",
            "cond": {"$gt": ["$$n", 0]},
            "limit": 2,
        }
    }
    assert evaluate(expr, {"nums": [1, 2, 3, 4]}) == [1, 2]


def test_map_transforms_each_element() -> None:
    expr = {"$map": {"input": "$nums", "as": "n", "in": {"$multiply": ["$$n", 10]}}}
    assert evaluate(expr, {"nums": [1, 2, 3]}) == [10, 20, 30]


def test_reduce_sums() -> None:
    expr = {
        "$reduce": {
            "input": "$nums",
            "initialValue": 0,
            "in": {"$add": ["$$value", "$$this"]},
        }
    }
    assert evaluate(expr, {"nums": [1, 2, 3, 4]}) == 10


def test_reduce_concat_string() -> None:
    expr = {
        "$reduce": {
            "input": "$tags",
            "initialValue": "",
            "in": {"$concat": ["$$value", "$$this"]},
        }
    }
    assert evaluate(expr, {"tags": ["a", "b", "c"]}) == "abc"


def test_root_system_var_resolves_to_doc() -> None:
    out = evaluate("$$ROOT", {"x": 1})
    assert out == {"x": 1}


def test_unknown_system_var_raises() -> None:
    from secantus.expressions import ExpressionError

    with pytest.raises(ExpressionError):
        evaluate("$$mystery", {})


def test_split() -> None:
    assert evaluate({"$split": ["a,b,c", ","]}, {}) == ["a", "b", "c"]
    assert evaluate({"$split": ["abc", ","]}, {}) == ["abc"]


def test_split_argument_validation() -> None:
    # mongod codes: empty separator 40087, non-string first/second 40085/40086,
    # wrong arg count 16020; a null string / separator -> null.
    for expr, code in [
        ({"$split": ["a,b", ""]}, 40087),
        ({"$split": [5, ","]}, 40085),
        ({"$split": ["a,b", 5]}, 40086),
        ({"$split": ["a,b"]}, 16020),
        ({"$split": ["a,b", ",", "x"]}, 16020),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {})
        assert exc.value.code == code, expr
    assert evaluate({"$split": [None, ","]}, {}) is None
    assert evaluate({"$split": ["a,b", None]}, {}) is None


def test_trim_default_whitespace() -> None:
    assert evaluate({"$trim": {"input": "  hi  "}}, {}) == "hi"


def test_trim_with_chars() -> None:
    assert evaluate({"$trim": {"input": "**hi**", "chars": "*"}}, {}) == "hi"


def test_ltrim_rtrim() -> None:
    assert evaluate({"$ltrim": {"input": "  hi  "}}, {}) == "hi  "
    assert evaluate({"$rtrim": {"input": "  hi  "}}, {}) == "  hi"


def test_trim_argument_validation() -> None:
    # mongod: non-string input -> 50699, non-string chars -> 50700; a null input
    # or null chars yields null.
    for op in ("$trim", "$ltrim", "$rtrim"):
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: {"input": 5}}, {})
        assert exc.value.code == 50699, op
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: {"input": "x", "chars": 5}}, {})
        assert exc.value.code == 50700, op
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: {"input": "x", "chars": True}}, {})
        assert exc.value.code == 50700, op
        assert evaluate({op: {"input": None}}, {}) is None
        assert evaluate({op: {"input": "--x--", "chars": None}}, {}) is None


def test_substr_cp() -> None:
    assert evaluate({"$substrCP": ["hello world", 6, 5]}, {}) == "world"
    assert evaluate({"$substrCP": ["hello world", 0, 5]}, {}) == "hello"


def test_str_len_cp() -> None:
    assert evaluate({"$strLenCP": "hello"}, {}) == 5


def test_index_of_cp() -> None:
    assert evaluate({"$indexOfCP": ["hello", "ll"]}, {}) == 2
    assert evaluate({"$indexOfCP": ["hello", "z"]}, {}) == -1
    assert evaluate({"$indexOfCP": ["hellohello", "ll", 4]}, {}) == 7


def test_index_of_start_end_validation() -> None:
    # mongod: a fractional / bool / non-numeric start or end is 40096, a negative
    # one is 40097; a whole double is accepted.
    for op in ("$indexOfBytes", "$indexOfCP"):
        assert evaluate({op: ["abcabc", "b", 2.0]}, {}) == 4  # whole double -> 2
        for bad in (2.5, True, "x"):
            with pytest.raises(ExpressionError) as exc:
                evaluate({op: ["abcabc", "b", bad]}, {})
            assert exc.value.code == 40096, (op, bad)
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: ["abcabc", "b", -1]}, {})
        assert exc.value.code == 40097, op
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: ["abcabc", "b", 0, -1]}, {})
        assert exc.value.code == 40097, op


def test_date_from_string_iso() -> None:
    import datetime as dt

    out = evaluate({"$dateFromString": {"dateString": "2026-04-28T13:14:15"}}, {})
    assert out == dt.datetime(2026, 4, 28, 13, 14, 15)


def test_date_from_string_with_format() -> None:
    import datetime as dt

    out = evaluate({"$dateFromString": {"dateString": "2026/04/28", "format": "%Y/%m/%d"}}, {})
    assert out == dt.datetime(2026, 4, 28)


def test_date_from_string_on_error_fallback() -> None:
    out = evaluate({"$dateFromString": {"dateString": "garbage", "onError": "FAILED"}}, {})
    assert out == "FAILED"


def test_date_to_string_with_iana_timezone() -> None:
    import datetime as dt

    when = dt.datetime(2026, 5, 2, 12, 0, 0, tzinfo=dt.timezone.utc)
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%H:%M %Z", "timezone": "Europe/Dublin"}},
        {"ts": when},
    )
    # Dublin is UTC+1 in May (BST).
    assert out.startswith("13:00 ")


def test_date_to_string_with_utc_offset() -> None:
    import datetime as dt

    when = dt.datetime(2026, 5, 2, 12, 0, 0, tzinfo=dt.timezone.utc)
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%H:%M", "timezone": "+05:30"}},
        {"ts": when},
    )
    assert out == "17:30"


def test_date_to_string_with_gmt_alias() -> None:
    import datetime as dt

    when = dt.datetime(2026, 5, 2, 12, 0, 0, tzinfo=dt.timezone.utc)
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%H:%M", "timezone": "GMT"}},
        {"ts": when},
    )
    assert out == "12:00"


def test_date_to_string_treats_naive_input_as_utc() -> None:
    import datetime as dt

    when = dt.datetime(2026, 5, 2, 12, 0, 0)  # naive
    out = evaluate(
        {"$dateToString": {"date": "$ts", "format": "%H:%M", "timezone": "+02:00"}},
        {"ts": when},
    )
    assert out == "14:00"


def test_date_from_string_with_timezone_interprets_naive_input() -> None:
    import datetime as dt

    out = evaluate(
        {
            "$dateFromString": {
                "dateString": "2026-05-02 12:00:00",
                "format": "%Y-%m-%d %H:%M:%S",
                "timezone": "+02:00",
            }
        },
        {},
    )
    # 12:00 in +02:00 represents the instant 10:00 UTC.
    assert out == dt.datetime(2026, 5, 2, 10, 0, 0, tzinfo=dt.timezone.utc)
    # Returned datetime is tz-aware in the requested zone.
    assert out.utcoffset() == dt.timedelta(hours=2)


def test_date_from_string_unknown_timezone_raises() -> None:
    from secantus.expressions import ExpressionError

    with pytest.raises(ExpressionError):
        evaluate(
            {"$dateFromString": {"dateString": "2026-05-02", "timezone": "Mars/Olympus"}},
            {},
        )


def test_date_from_string_on_null_fallback() -> None:
    out = evaluate({"$dateFromString": {"dateString": None, "onNull": "missing"}}, {})
    assert out == "missing"


def test_abs_round_floor_ceil() -> None:
    assert evaluate({"$abs": -5}, {}) == 5
    assert evaluate({"$abs": -3.7}, {}) == 3.7
    assert evaluate({"$round": [3.7, 0]}, {}) == 4
    assert evaluate({"$round": [3.456, 1]}, {}) == 3.5
    assert evaluate({"$floor": 3.7}, {}) == 3
    assert evaluate({"$ceil": 3.2}, {}) == 4


def test_unary_math_rejects_non_numeric() -> None:
    # mongod: a string / bool operand is rejected (28765 for most, 51081 for
    # $round / $trunc), not coerced or leaked as a Python error; null -> null.
    for op in ("$abs", "$ceil", "$floor", "$sqrt", "$exp", "$ln", "$log10"):
        for bad in ("x", True):
            with pytest.raises(ExpressionError) as exc:
                evaluate({op: bad}, {})
            assert exc.value.code == 28765, f"{op}({bad!r})"
        assert evaluate({op: None}, {}) is None
    for op in ("$round", "$trunc"):
        for bad in ("x", True):
            with pytest.raises(ExpressionError) as exc:
                evaluate({op: [bad, 0]}, {})
            assert exc.value.code == 51081, f"{op}({bad!r})"
        assert evaluate({op: None}, {}) is None


def test_sqrt_pow() -> None:
    assert evaluate({"$sqrt": 9}, {}) == 3
    # Out-of-domain now raises mongod's Location28714 (was null).
    with pytest.raises(ExpressionError):
        evaluate({"$sqrt": -1}, {})
    assert evaluate({"$pow": [2, 8]}, {}) == 256


def test_log_family() -> None:
    import math

    assert evaluate({"$ln": math.e}, {}) == pytest.approx(1)
    assert evaluate({"$log": [100, 10]}, {}) == pytest.approx(2)
    assert evaluate({"$log10": 1000}, {}) == pytest.approx(3)
    # Out-of-domain now raises mongod's Location28766 (was null) — see
    # test_log_family_domain_errors for the full matrix.
    with pytest.raises(ExpressionError):
        evaluate({"$ln": -1}, {})


def test_trunc() -> None:
    assert evaluate({"$trunc": 3.7}, {}) == 3
    assert evaluate({"$trunc": [3.456, 1]}, {}) == pytest.approx(3.4)


def test_merge_objects() -> None:
    assert evaluate({"$mergeObjects": [{"a": 1, "b": 2}, {"b": 99, "c": 3}]}, {}) == {
        "a": 1,
        "b": 99,
        "c": 3,
    }
    assert evaluate({"$mergeObjects": [{"a": 1}, None, {"b": 2}]}, {}) == {"a": 1, "b": 2}


def test_object_to_array() -> None:
    out = evaluate({"$objectToArray": "$d"}, {"d": {"a": 1, "b": 2}})
    assert out == [{"k": "a", "v": 1}, {"k": "b", "v": 2}]


def test_array_to_object_kv_form() -> None:
    arr = [{"k": "a", "v": 1}, {"k": "b", "v": 2}]
    out = evaluate({"$arrayToObject": "$a"}, {"a": arr})
    assert out == {"a": 1, "b": 2}


def test_array_to_object_pair_form() -> None:
    arr = [["a", 1], ["b", 2]]
    out = evaluate({"$arrayToObject": "$a"}, {"a": arr})
    assert out == {"a": 1, "b": 2}


def test_object_array_round_trip() -> None:
    expr = {"$arrayToObject": {"$objectToArray": "$d"}}
    out = evaluate(expr, {"d": {"a": 1, "b": 2}})
    assert out == {"a": 1, "b": 2}


def test_switch_first_matching_branch() -> None:
    expr = {
        "$switch": {
            "branches": [
                {"case": {"$lt": ["$x", 0]}, "then": "negative"},
                {"case": {"$eq": ["$x", 0]}, "then": "zero"},
            ],
            "default": "positive",
        }
    }
    assert evaluate(expr, {"x": -3}) == "negative"
    assert evaluate(expr, {"x": 0}) == "zero"
    assert evaluate(expr, {"x": 5}) == "positive"


def test_switch_no_default_raises() -> None:
    from secantus.expressions import ExpressionError

    expr = {"$switch": {"branches": [{"case": False, "then": "x"}]}}
    with pytest.raises(ExpressionError):
        evaluate(expr, {})


def test_regex_match() -> None:
    assert evaluate({"$regexMatch": {"input": "hello", "regex": "^he"}}, {}) is True
    assert evaluate({"$regexMatch": {"input": "hello", "regex": "^he$"}}, {}) is False
    assert (
        evaluate({"$regexMatch": {"input": "HELLO", "regex": "hello", "options": "i"}}, {}) is True
    )


def test_regex_find_returns_match_dict() -> None:
    out = evaluate({"$regexFind": {"input": "abc123", "regex": r"(\w+)(\d+)"}}, {})
    assert out["match"] == "abc123"
    assert out["idx"] == 0
    assert out["captures"] == ["abc12", "3"]


def test_regex_find_no_match_returns_none() -> None:
    assert evaluate({"$regexFind": {"input": "hello", "regex": "z+"}}, {}) is None


def test_regex_find_all() -> None:
    out = evaluate({"$regexFindAll": {"input": "a1 b22 c333", "regex": r"\d+"}}, {})
    assert [m["match"] for m in out] == ["1", "22", "333"]


def test_date_add_days() -> None:
    import datetime as dt

    base = dt.datetime(2026, 4, 28, 12, 0, 0)
    out = evaluate({"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 7}}, {"d": base})
    assert out == dt.datetime(2026, 5, 5, 12, 0, 0)


def test_date_arg_validation() -> None:
    import datetime as dt

    d = dt.datetime(2021, 1, 1)
    # Whole-double amount / binSize is accepted (coerced to int).
    assert evaluate(
        {"$dateAdd": {"startDate": d, "unit": "day", "amount": 2.0}}, {}
    ) == dt.datetime(2021, 1, 3)
    assert evaluate({"$dateTrunc": {"date": d, "unit": "year", "binSize": 2.0}}, {}) == dt.datetime(
        2021, 1, 1
    )
    # $dateAdd / $dateSubtract amount: fractional / bool / non-numeric -> 5166405.
    for op in ("$dateAdd", "$dateSubtract"):
        for bad in (2.5, True, "x"):
            with pytest.raises(ExpressionError) as exc:
                evaluate({op: {"startDate": d, "unit": "day", "amount": bad}}, {})
            assert exc.value.code == 5166405, (op, bad)
    # $dateTrunc binSize: non-integer -> 5439017, non-positive -> 5439018.
    for bad in (2.5, True):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$dateTrunc": {"date": d, "unit": "day", "binSize": bad}}, {})
        assert exc.value.code == 5439017, bad
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$dateTrunc": {"date": d, "unit": "day", "binSize": -1}}, {})
    assert exc.value.code == 5439018


def test_date_add_months_with_day_clamp() -> None:
    import datetime as dt

    base = dt.datetime(2026, 1, 31)
    out = evaluate({"$dateAdd": {"startDate": "$d", "unit": "month", "amount": 1}}, {"d": base})
    assert out == dt.datetime(2026, 2, 28)


def test_date_subtract() -> None:
    import datetime as dt

    base = dt.datetime(2026, 4, 28)
    out = evaluate(
        {"$dateSubtract": {"startDate": "$d", "unit": "year", "amount": 1}},
        {"d": base},
    )
    assert out == dt.datetime(2025, 4, 28)


def test_date_diff_units() -> None:
    import datetime as dt

    a = dt.datetime(2025, 1, 1)
    b = dt.datetime(2026, 4, 28)
    spec = {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "year"}}
    assert evaluate(spec, {"a": a, "b": b}) == 1
    spec_month = {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "month"}}
    assert evaluate(spec_month, {"a": a, "b": b}) == 15
    spec_day = {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "day"}}
    assert evaluate(spec_day, {"a": a, "b": b}) == (b - a).days


def test_index_of_array() -> None:
    assert evaluate({"$indexOfArray": [["a", "b", "c"], "b"]}, {}) == 1
    assert evaluate({"$indexOfArray": [["a", "b", "c"], "z"]}, {}) == -1
    assert evaluate({"$indexOfArray": [["a", "b", "c", "b"], "b", 2]}, {}) == 3


def test_index_of_bytes_and_str_len_bytes() -> None:
    assert evaluate({"$indexOfBytes": ["hello", "ll"]}, {}) == 2
    assert evaluate({"$strLenBytes": "héllo"}, {}) == 6  # é is 2 UTF-8 bytes


def test_substr_bytes() -> None:
    assert evaluate({"$substrBytes": ["hello world", 6, 5]}, {}) == "world"


def test_let_binds_variables() -> None:
    expr = {
        "$let": {
            "vars": {"a": 5, "b": {"$multiply": ["$x", 2]}},
            "in": {"$add": ["$$a", "$$b"]},
        }
    }
    assert evaluate(expr, {"x": 10}) == 25


def test_range_default_step() -> None:
    assert evaluate({"$range": [0, 5]}, {}) == [0, 1, 2, 3, 4]


def test_range_with_step() -> None:
    assert evaluate({"$range": [0, 10, 3]}, {}) == [0, 3, 6, 9]


def test_zip_min_length() -> None:
    out = evaluate({"$zip": {"inputs": [[1, 2, 3], ["a", "b"]]}}, {})
    assert out == [[1, "a"], [2, "b"]]


def test_zip_longest_with_defaults() -> None:
    out = evaluate(
        {
            "$zip": {
                "inputs": [[1, 2, 3], ["a"]],
                "useLongestLength": True,
                "defaults": [0, "?"],
            }
        },
        {},
    )
    assert out == [[1, "a"], [2, "?"], [3, "?"]]


def test_sort_array_scalar() -> None:
    assert evaluate({"$sortArray": {"input": [3, 1, 2], "sortBy": 1}}, {}) == [1, 2, 3]
    assert evaluate({"$sortArray": {"input": [3, 1, 2], "sortBy": -1}}, {}) == [3, 2, 1]


def test_sort_array_by_doc_field() -> None:
    arr = [{"n": 3}, {"n": 1}, {"n": 2}]
    out = evaluate({"$sortArray": {"input": "$arr", "sortBy": {"n": 1}}}, {"arr": arr})
    assert [d["n"] for d in out] == [1, 2, 3]


def test_date_trunc_to_month() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 28, 13, 14, 15)
    out = evaluate({"$dateTrunc": {"date": "$d", "unit": "month"}}, {"d": when})
    assert out == dt.datetime(2026, 4, 1)


def test_date_trunc_to_hour_with_bin_size() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 28, 13, 47, 0)
    out = evaluate({"$dateTrunc": {"date": "$d", "unit": "hour", "binSize": 6}}, {"d": when})
    assert out == dt.datetime(2026, 4, 28, 12, 0, 0)


def test_date_trunc_to_day() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 28, 13, 14, 15)
    out = evaluate({"$dateTrunc": {"date": "$d", "unit": "day"}}, {"d": when})
    assert out == dt.datetime(2026, 4, 28)


def test_date_to_parts() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 28, 13, 14, 15, 678000)
    out = evaluate({"$dateToParts": {"date": "$d"}}, {"d": when})
    assert out == {
        "year": 2026,
        "month": 4,
        "day": 28,
        "hour": 13,
        "minute": 14,
        "second": 15,
        "millisecond": 678,
    }


def test_date_component_extractors_more() -> None:
    import datetime as dt

    # 2026-03-15 is a Sunday.
    when = dt.datetime(2026, 3, 15, 10, 30, 45, 123000)
    doc = {"d": when}
    assert evaluate({"$dayOfYear": "$d"}, doc) == 74
    assert evaluate({"$week": "$d"}, doc) == 11
    assert evaluate({"$isoWeek": "$d"}, doc) == 11
    assert evaluate({"$isoDayOfWeek": "$d"}, doc) == 7  # Sunday
    assert evaluate({"$isoWeekYear": "$d"}, doc) == 2026
    assert evaluate({"$millisecond": "$d"}, doc) == 123


def test_day_of_year_boundaries() -> None:
    import datetime as dt

    assert evaluate({"$dayOfYear": "$d"}, {"d": dt.datetime(2026, 1, 1)}) == 1
    # 2024 is a leap year.
    assert evaluate({"$dayOfYear": "$d"}, {"d": dt.datetime(2024, 12, 31)}) == 366


def test_us_week_boundaries() -> None:
    import datetime as dt

    # 2026-01-01 is a Thursday -> before the first Sunday -> week 0.
    assert evaluate({"$week": "$d"}, {"d": dt.datetime(2026, 1, 1)}) == 0
    # 2023-01-01 is a Sunday -> that Sunday starts week 1.
    assert evaluate({"$week": "$d"}, {"d": dt.datetime(2023, 1, 1)}) == 1


def test_iso_week_year_boundary() -> None:
    import datetime as dt

    # 2026-01-01 is a Thursday -> belongs to ISO year 2026, week 1.
    d0 = dt.datetime(2026, 1, 1)
    assert evaluate({"$isoWeek": "$d"}, {"d": d0}) == 1
    assert evaluate({"$isoWeekYear": "$d"}, {"d": d0}) == 2026
    # 2027-01-01 is a Friday -> belongs to ISO year 2026, week 53.
    d1 = dt.datetime(2027, 1, 1)
    assert evaluate({"$isoWeek": "$d"}, {"d": d1}) == 53
    assert evaluate({"$isoWeekYear": "$d"}, {"d": d1}) == 2026
    assert evaluate({"$isoDayOfWeek": "$d"}, {"d": d1}) == 5  # Friday


def test_date_extractors_with_timezone() -> None:
    import datetime as dt

    # 2026-03-15T02:00Z. In a -05:00 fixed offset zone this is 2026-03-14 21:00
    # (previous day), crossing a day boundary.
    when = dt.datetime(2026, 3, 15, 2, 0, 0, tzinfo=dt.timezone.utc)
    doc = {"d": when}
    assert evaluate({"$dayOfMonth": {"date": "$d", "timezone": "-05:00"}}, doc) == 14
    assert evaluate({"$dayOfYear": {"date": "$d", "timezone": "-05:00"}}, doc) == 73
    assert evaluate({"$isoDayOfWeek": {"date": "$d", "timezone": "-05:00"}}, doc) == 6  # Saturday
    # Named IANA zone (America/New_York, EDT -04:00 in March) also shifts to the 14th.
    assert evaluate({"$dayOfMonth": {"date": "$d", "timezone": "America/New_York"}}, doc) == 14
    assert evaluate({"$isoWeek": {"date": "$d", "timezone": "America/New_York"}}, doc) == 11


def test_date_extractors_null_and_missing() -> None:
    # All 13 extractors: null / missing -> null; a non-date value -> Location16006.
    for op in (
        "$year",
        "$month",
        "$dayOfMonth",
        "$hour",
        "$minute",
        "$second",
        "$dayOfWeek",
        "$dayOfYear",
        "$week",
        "$isoWeek",
        "$isoDayOfWeek",
        "$isoWeekYear",
        "$millisecond",
    ):
        assert evaluate({op: "$d"}, {"d": None}) is None
        assert evaluate({op: "$missing"}, {}) is None
        for bad in ("not a date", 5):
            with pytest.raises(ExpressionError) as exc:
                evaluate({op: "$d"}, {"d": bad})
            assert exc.value.code == 16006


def test_date_to_parts_iso8601() -> None:
    import datetime as dt

    when = dt.datetime(2027, 1, 1, 13, 14, 15, 678000)  # Friday -> ISO 2026/W53/5
    assert evaluate({"$dateToParts": {"date": "$d", "iso8601": True}}, {"d": when}) == {
        "isoWeekYear": 2026,
        "isoWeek": 53,
        "isoDayOfWeek": 5,
        "hour": 13,
        "minute": 14,
        "second": 15,
        "millisecond": 678,
    }
    # iso8601: false is the existing non-ISO shape, unchanged.
    assert evaluate({"$dateToParts": {"date": "$d", "iso8601": False}}, {"d": when}) == {
        "year": 2027,
        "month": 1,
        "day": 1,
        "hour": 13,
        "minute": 14,
        "second": 15,
        "millisecond": 678,
    }


def test_date_to_parts_iso8601_with_timezone() -> None:
    import datetime as dt

    # 2027-01-01T02:00Z -> in -05:00 this is 2026-12-31 21:00 (Thursday) -> ISO 2026/W53/4
    when = dt.datetime(2027, 1, 1, 2, 0, 0, tzinfo=dt.timezone.utc)
    out = evaluate(
        {"$dateToParts": {"date": "$d", "iso8601": True, "timezone": "-05:00"}}, {"d": when}
    )
    assert out["isoWeekYear"] == 2026
    assert out["isoWeek"] == 53
    assert out["isoDayOfWeek"] == 4  # Thursday
    assert out["hour"] == 21


def test_convert_string_to_int() -> None:
    assert evaluate({"$convert": {"input": "42", "to": "int"}}, {}) == 42


def test_convert_with_on_error() -> None:
    out = evaluate({"$convert": {"input": "not a number", "to": "int", "onError": -1}}, {})
    assert out == -1


def test_convert_with_on_null() -> None:
    out = evaluate({"$convert": {"input": None, "to": "int", "onNull": 0}}, {})
    assert out == 0


def test_convert_int_to_string() -> None:
    assert evaluate({"$convert": {"input": 42, "to": "string"}}, {}) == "42"


def test_convert_to_objectid() -> None:
    from bson import ObjectId

    hex_id = "507f1f77bcf86cd799439011"
    out = evaluate({"$convert": {"input": hex_id, "to": "objectId"}}, {})
    assert isinstance(out, ObjectId)
    assert str(out) == hex_id


def test_convert_to_date_from_string() -> None:
    import datetime as dt

    out = evaluate({"$convert": {"input": "2026-04-28T12:00:00", "to": "date"}}, {})
    assert out == dt.datetime(2026, 4, 28, 12, 0, 0)


def test_to_date_noop_on_date() -> None:
    import datetime as dt

    d = dt.datetime(2020, 1, 1, 0, 0, 0)
    assert evaluate({"$toDate": d}, {}) == d


def test_to_date_from_int_millis() -> None:
    import datetime as dt

    # milliseconds since the Unix epoch -> 2023-11-14T22:13:20Z
    out = evaluate({"$toDate": 1700000000000}, {})
    assert out == dt.datetime(2023, 11, 14, 22, 13, 20, tzinfo=dt.timezone.utc)


def test_to_date_from_string() -> None:
    import datetime as dt

    out = evaluate({"$toDate": "2026-04-28T12:00:00"}, {})
    assert out == dt.datetime(2026, 4, 28, 12, 0, 0)


def test_to_date_null_is_null() -> None:
    assert evaluate({"$toDate": None}, {}) is None


def test_to_date_rejects_bool() -> None:
    # mongod: bool -> date is ConversionFailure (241), not an int coercion. The
    # same holds through $convert (whose onError still catches it).
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$toDate": True}, {})
    assert exc.value.code == 241
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$convert": {"input": True, "to": "date"}}, {})
    assert exc.value.code == 241
    assert evaluate({"$convert": {"input": True, "to": "date", "onError": "x"}}, {}) == "x"


def test_trig_basic() -> None:
    assert evaluate({"$sin": 0}, {}) == 0.0
    assert evaluate({"$cos": 0}, {}) == 1.0
    assert evaluate({"$asin": 1}, {}) == math.pi / 2
    assert evaluate({"$atan2": [1, 1]}, {}) == math.pi / 4
    assert evaluate({"$acosh": 1}, {}) == 0.0


def test_trig_atanh_edges() -> None:
    assert evaluate({"$atanh": 1}, {}) == math.inf
    assert evaluate({"$atanh": -1}, {}) == -math.inf


def test_trig_null_propagation() -> None:
    assert evaluate({"$sin": None}, {}) is None
    assert evaluate({"$sin": "$missing"}, {}) is None
    assert evaluate({"$atan2": [None, 1]}, {}) is None


def test_trig_domain_errors() -> None:
    for expr in ({"$asin": 5}, {"$acos": -2}, {"$acosh": 0.5}, {"$atanh": 2}):
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {})
        assert exc.value.code == 50989
    # sin/cos/tan reject non-finite.
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$sin": math.inf}, {})
    assert exc.value.code == 50989


def test_trig_type_errors() -> None:
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$sin": "hi"}, {})
    assert exc.value.code == 28765
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$atan2": ["hi", 1]}, {})
    assert exc.value.code == 51044


def test_get_field_absent_is_missing() -> None:
    from secantus.expressions import MISSING

    # A field absent from the input document resolves to the MISSING marker.
    assert evaluate({"$getField": {"field": "k", "input": "$sub"}}, {"sub": {"j": 2}}) is MISSING
    # An input path that is itself missing also resolves to MISSING (mongod 6.0).
    assert evaluate({"$getField": {"field": "k", "input": "$sub"}}, {}) is MISSING


def test_get_field_null_input_is_null() -> None:
    from secantus.expressions import MISSING

    # An input that resolves to an explicit null yields null (NOT missing) —
    # verified against mongod 6.0.
    got = evaluate({"$getField": {"field": "k", "input": "$sub"}}, {"sub": None})
    assert got is None
    assert got is not MISSING
    got2 = evaluate({"$getField": {"field": "k", "input": {"$literal": None}}}, {})
    assert got2 is None
    assert got2 is not MISSING


def test_get_field_present_null_is_null() -> None:
    # A field present with an explicit null returns null, NOT the MISSING marker.
    from secantus.expressions import MISSING

    got = evaluate({"$getField": {"field": "k", "input": "$sub"}}, {"sub": {"k": None}})
    assert got is None
    assert got is not MISSING


def test_get_field_present_value() -> None:
    assert evaluate({"$getField": {"field": "k", "input": "$sub"}}, {"sub": {"k": 1}}) == 1


def test_project_getfield_missing_is_omitted() -> None:
    from secantus.aggregate import PipelineContext, apply_pipeline

    ctx = PipelineContext(storage=None, db_name="t", vars={})  # type: ignore[arg-type]
    docs = [
        {"_id": 1, "sub": {"k": 1}},
        {"_id": 2, "sub": {"j": 2}},
        {"_id": 5},
    ]
    out = apply_pipeline(
        docs, [{"$project": {"r": {"$getField": {"field": "k", "input": "$sub"}}}}], ctx
    )
    # _id:1 -> r present; _id:2 and _id:5 -> r omitted entirely (not null).
    assert out == [{"_id": 1, "r": 1}, {"_id": 2}, {"_id": 5}]


def test_project_getfield_present_null_emitted() -> None:
    from secantus.aggregate import PipelineContext, apply_pipeline

    ctx = PipelineContext(storage=None, db_name="t", vars={})  # type: ignore[arg-type]
    out = apply_pipeline(
        [{"_id": 1, "sub": {"k": None}}],
        [{"$project": {"r": {"$getField": {"field": "k", "input": "$sub"}}}}],
        ctx,
    )
    assert out == [{"_id": 1, "r": None}]


def test_addfields_getfield_missing_is_dropped() -> None:
    from secantus.aggregate import PipelineContext, apply_pipeline

    ctx = PipelineContext(storage=None, db_name="t", vars={})  # type: ignore[arg-type]
    # Absent field -> the target is not written (existing values untouched).
    out = apply_pipeline(
        [{"_id": 1, "sub": {"j": 2}}],
        [{"$addFields": {"r": {"$getField": {"field": "k", "input": "$sub"}}}}],
        ctx,
    )
    assert out == [{"_id": 1, "sub": {"j": 2}}]
    # An existing field set to $$REMOVE is removed (mongod semantics).
    out2 = apply_pipeline(
        [{"_id": 1, "keep": 9, "drop": 3}],
        [{"$addFields": {"drop": "$$REMOVE"}}],
        ctx,
    )
    assert out2 == [{"_id": 1, "keep": 9}]


def test_project_remove_sentinel_is_omitted() -> None:
    from secantus.aggregate import PipelineContext, apply_pipeline

    ctx = PipelineContext(storage=None, db_name="t", vars={})  # type: ignore[arg-type]
    out = apply_pipeline([{"_id": 1, "a": 1}], [{"$project": {"r": "$$REMOVE"}}], ctx)
    assert out == [{"_id": 1}]


def test_log_family_domain_errors() -> None:
    """Out-of-domain log/sqrt args raise mongod's Location codes (probed
    against mongod 7.0.12) instead of returning null; null/missing still pass
    through as null, and NaN propagates as nan."""
    import math

    import pytest

    from secantus.expressions import ExpressionError, evaluate

    for expr, code in [
        ({"$ln": 0}, 28766),
        ({"$ln": -1}, 28766),
        ({"$log10": 0}, 28761),
        ({"$log10": -2.5}, 28761),
        ({"$log": [0, 2]}, 28758),
        ({"$log": [-1, 2]}, 28758),
        ({"$log": [8, 0]}, 28759),
        ({"$log": [8, 1]}, 28759),
        ({"$log": [8, -2]}, 28759),
        ({"$sqrt": -1}, 28714),
        ({"$sqrt": -0.5}, 28714),
        # $log type errors (28756 argument / 28757 base) precede the domain check.
        ({"$log": ["x", 2]}, 28756),
        ({"$log": [True, 2]}, 28756),
        ({"$log": [8, "y"]}, 28757),
        ({"$log": [8, True]}, 28757),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {}, None)
        assert exc.value.code == code, expr

    assert evaluate({"$ln": None}, {}, None) is None
    assert evaluate({"$ln": "$missing"}, {}, None) is None
    assert evaluate({"$sqrt": 0}, {}, None) == 0.0
    assert math.isnan(evaluate({"$ln": float("nan")}, {}, None))


def test_bool_argument_rejected_where_int_expected() -> None:
    # mongod rejects a bool where an aggregation operator expects a numeric
    # (int) argument — bool is not a number. Each carries mongod's exact code.
    for expr, code in [
        ({"$round": [1.5, True]}, 16004),
        ({"$trunc": [1.5, True]}, 16004),
        ({"$arrayElemAt": [[10, 20, 30], True]}, 28690),
        ({"$slice": [[1, 2, 3, 4], True]}, 28725),
        ({"$slice": [[1, 2, 3, 4], True, 2]}, 28725),
        ({"$slice": [[1, 2, 3, 4], 1, True]}, 28727),
        ({"$sortArray": {"input": [3, 1, 2], "sortBy": True}}, 2942507),
        ({"$substrCP": ["hello", True, 2]}, 34450),
        ({"$substrCP": ["hello", 1, True]}, 34452),
        ({"$substrBytes": ["hello", True, 2]}, 16034),
        ({"$substrBytes": ["hello", 1, True]}, 16035),
        ({"$substr": ["hello", True, 2]}, 16034),  # $substr aliases $substrBytes
        ({"$substr": ["hello", 1, True]}, 16035),
        ({"$range": [True, 5]}, 34443),
        ({"$range": [0, True]}, 34445),
        ({"$range": [0, 5, True]}, 34447),
        ({"$indexOfArray": [[1, 2, 3], 2, True]}, 40096),
        ({"$indexOfArray": [[1, 2, 3], 2, 0, True]}, 40096),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {}, None)
        assert exc.value.code == code, expr

    # A plain int argument still computes (the guard is bool-specific).
    assert evaluate({"$arrayElemAt": [[10, 20, 30], 1]}, {}, None) == 20
    assert evaluate({"$slice": [[1, 2, 3, 4], 2]}, {}, None) == [1, 2]


def test_whole_number_double_index_accepted_fractional_rejected() -> None:
    # mongod accepts a whole-number double where an int index is expected
    # (coerced to int) and rejects a fractional double with a per-op code.
    assert evaluate({"$arrayElemAt": [[10, 20, 30], 2.0]}, {}, None) == 30
    assert evaluate({"$arrayElemAt": [[10, 20, 30], -1.0]}, {}, None) == 30
    assert evaluate({"$slice": [[1, 2, 3, 4], 2.0]}, {}, None) == [1, 2]
    assert evaluate({"$slice": [[1, 2, 3, 4], 1.0, 2.0]}, {}, None) == [2, 3]
    assert evaluate({"$indexOfArray": [[1, 2, 3], 2, 0.0]}, {}, None) == 1
    assert evaluate({"$substrCP": ["hello", 1.0, 2]}, {}, None) == "el"
    assert evaluate({"$range": [0.0, 5.0, 1.0]}, {}, None) == [0, 1, 2, 3, 4]
    assert evaluate({"$round": [3.14159, 2.0]}, {}, None) == 3.14
    assert evaluate({"$trunc": [3.14159, 2.0]}, {}, None) == 3.14

    for expr, code in [
        ({"$arrayElemAt": [[10, 20, 30], 2.7]}, 28691),
        ({"$slice": [[1, 2, 3, 4], 2.7]}, 28726),
        ({"$slice": [[1, 2, 3, 4], 1.7, 2]}, 28726),
        ({"$slice": [[1, 2, 3, 4], 1, 1.7]}, 28728),
        ({"$indexOfArray": [[1, 2, 3], 2, 0.7]}, 40096),
        ({"$indexOfArray": [[1, 2, 3], 2, 0, 0.7]}, 40096),
        ({"$substrCP": ["hello", 1.7, 2]}, 34451),
        ({"$substrCP": ["hello", 1, 1.7]}, 34453),
        ({"$range": [0.7, 5]}, 34444),
        ({"$range": [0, 5.7]}, 34446),
        ({"$range": [0, 5, 1.7]}, 34448),
        ({"$round": [3.14159, 2.7]}, 51082),
        ({"$trunc": [3.14159, 2.7]}, 51082),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {}, None)
        assert exc.value.code == code, expr


def test_substr_bytes_rejects_split_utf8_character() -> None:
    # mongod rejects a $substrBytes range that splits a UTF-8 character rather
    # than returning a replacement char. "héllo": é is bytes 1-2.
    with pytest.raises(ExpressionError) as end_exc:
        evaluate({"$substrBytes": ["héllo", 0, 2]}, {}, None)  # ends inside é
    assert end_exc.value.code == 28657
    with pytest.raises(ExpressionError) as start_exc:
        evaluate({"$substrBytes": ["héllo", 2, 3]}, {}, None)  # starts inside é
    assert start_exc.value.code == 28656
    # $substr is the byte-based alias — same rejection.
    with pytest.raises(ExpressionError) as alias_exc:
        evaluate({"$substr": ["héllo", 0, 2]}, {}, None)
    assert alias_exc.value.code == 28657
    # A continuation-byte start is rejected even for an empty (length 0) range.
    with pytest.raises(ExpressionError) as empty_exc:
        evaluate({"$substrBytes": ["héllo", 2, 0]}, {}, None)
    assert empty_exc.value.code == 28656
    # Clean boundaries and clamped/past-end ranges still compute (byte 1 is é's
    # lead byte — a valid boundary — so [1, 0] is an empty slice, not an error).
    assert evaluate({"$substrBytes": ["héllo", 1, 0]}, {}, None) == ""
    assert evaluate({"$substrBytes": ["héllo", 0, 3]}, {}, None) == "hé"
    assert evaluate({"$substrBytes": ["héllo", 3, 2]}, {}, None) == "ll"
    assert evaluate({"$substrBytes": ["héllo", 3, 99]}, {}, None) == "llo"
    assert evaluate({"$substrBytes": ["héllo", 99, 0]}, {}, None) == ""


def test_substr_negative_index_rejected() -> None:
    # mongod rejects a negative start for both $substr* ops, and a negative
    # length for $substrCP (a negative length is fine for $substrBytes).
    for expr, code in [
        ({"$substrBytes": ["abcde", -1, 2]}, 50752),
        ({"$substr": ["abcde", -1, 2]}, 50752),
        ({"$substrCP": ["abcde", -1, 2]}, 34455),
        ({"$substrCP": ["abcde", 1, -1]}, 34454),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {}, None)
        assert exc.value.code == code, expr
    # $substrBytes negative length still means "to the end".
    assert evaluate({"$substrBytes": ["abcde", 1, -1]}, {}, None) == "bcde"


def test_substr_bytes_truncates_double_index() -> None:
    # Unlike $substrCP, mongod's $substrBytes accepts any double and truncates
    # toward zero (then the usual negative-start / to-end rules apply).
    assert evaluate({"$substrBytes": ["abcde", 1.7, 2]}, {}, None) == "bc"
    assert evaluate({"$substrBytes": ["abcde", 0.9, 3]}, {}, None) == "abc"
    assert evaluate({"$substrBytes": ["abcde", 1, 2.9]}, {}, None) == "bc"
    assert evaluate({"$substrBytes": ["abcde", 1, -1.7]}, {}, None) == "bcde"
    # A truncated-negative start is still rejected (−1.7 → −1 → 50752).
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$substrBytes": ["abcde", -1.7, 2]}, {}, None)
    assert exc.value.code == 50752


def test_pow_domain_and_type_validation() -> None:
    """$pow: a negative base with a fractional exponent returns NaN (not an
    unencodable Python complex), and a non-numeric operand / bool / a zero base
    with a negative exponent raise mongod's codes. mongod 7.0.12-verified."""
    import bson

    r = evaluate({"$pow": [-2, 0.5]}, {}, None)
    assert isinstance(r, float) and math.isnan(r)
    bson.encode({"r": r})  # must be encodable (regression: was a complex -> crash)
    assert evaluate({"$pow": [-2, 3]}, {}, None) == -8
    assert evaluate({"$pow": [2, 10]}, {}, None) == 1024
    assert evaluate({"$pow": ["$missing", 2]}, {}, None) is None
    for expr, code in [
        ({"$pow": ["x", 2]}, 28762),
        ({"$pow": [True, 2]}, 28762),
        ({"$pow": [2, "x"]}, 28763),
        ({"$pow": [2, True]}, 28763),
        ({"$pow": [0, -1]}, 28764),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {}, None)
        assert exc.value.code == code, expr
