from __future__ import annotations

import math

import pytest

from secantus.expressions import ExpressionError, evaluate


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
    assert evaluate({"$concat": ["x=", "$x"]}, {"x": 5}) == "x=5"


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
    assert evaluate({"$arrayElemAt": ["$a", 99]}, doc) is None


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


def test_in_operator_in_expressions() -> None:
    assert evaluate({"$in": ["b", ["a", "b", "c"]]}, {}) is True
    assert evaluate({"$in": ["x", ["a", "b", "c"]]}, {}) is False


def test_to_int_conversions() -> None:
    assert evaluate({"$toInt": 3.7}, {}) == 3
    assert evaluate({"$toInt": "42"}, {}) == 42
    assert evaluate({"$toInt": True}, {}) == 1


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


def test_trim_default_whitespace() -> None:
    assert evaluate({"$trim": {"input": "  hi  "}}, {}) == "hi"


def test_trim_with_chars() -> None:
    assert evaluate({"$trim": {"input": "**hi**", "chars": "*"}}, {}) == "hi"


def test_ltrim_rtrim() -> None:
    assert evaluate({"$ltrim": {"input": "  hi  "}}, {}) == "hi  "
    assert evaluate({"$rtrim": {"input": "  hi  "}}, {}) == "  hi"


def test_substr_cp() -> None:
    assert evaluate({"$substrCP": ["hello world", 6, 5]}, {}) == "world"
    assert evaluate({"$substrCP": ["hello world", 0, 5]}, {}) == "hello"


def test_str_len_cp() -> None:
    assert evaluate({"$strLenCP": "hello"}, {}) == 5


def test_index_of_cp() -> None:
    assert evaluate({"$indexOfCP": ["hello", "ll"]}, {}) == 2
    assert evaluate({"$indexOfCP": ["hello", "z"]}, {}) == -1
    assert evaluate({"$indexOfCP": ["hellohello", "ll", 4]}, {}) == 7


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

    for expr, code in [
        ({"$arrayElemAt": [[10, 20, 30], 2.7]}, 28691),
        ({"$slice": [[1, 2, 3, 4], 2.7]}, 28726),
        ({"$slice": [[1, 2, 3, 4], 1.7, 2]}, 28726),
        ({"$slice": [[1, 2, 3, 4], 1, 1.7]}, 28728),
        ({"$indexOfArray": [[1, 2, 3], 2, 0.7]}, 40096),
        ({"$indexOfArray": [[1, 2, 3], 2, 0, 0.7]}, 40096),
    ]:
        with pytest.raises(ExpressionError) as exc:
            evaluate(expr, {}, None)
        assert exc.value.code == code, expr
