"""Date operators honour their `timezone`, and bin from mongod's reference.

Every value here is pinned against mongod 8.2.11 (2026-09-01). The backlog
carried this corner as one Rust-only gap in `$dateFromString`; measuring it
found two silent WRONG ANSWERS on both servers instead.

`$dateTrunc` never read `timezone` at all, so a daily rollup for
`America/New_York` bucketed at 00:00Z rather than 04:00Z -- four hours of every
day attributed to the wrong bucket, with nothing raised. `$dateDiff` ignored it
too, and computed "whole units elapsed" rather than mongod's boundary crossings.
"""

import datetime as dt

import pytest

from secantus.expressions import ExpressionError, evaluate

D = dt.datetime(2026, 7, 4, 15, 30, 45, 123000)  # a Saturday, 15:30:45.123 UTC


def trunc(unit, **kw):
    return evaluate({"$dateTrunc": {"date": "$d", "unit": unit, **kw}}, {"d": D})


def diff(a, b, unit, **kw):
    return evaluate(
        {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": unit, **kw}},
        {"a": a, "b": b},
    )


class TestTruncateInTheZone:
    @pytest.mark.parametrize(
        ("unit", "tz", "expected"),
        [
            ("day", "UTC", dt.datetime(2026, 7, 4)),
            ("day", "America/New_York", dt.datetime(2026, 7, 4, 4)),
            ("day", "Asia/Kolkata", dt.datetime(2026, 7, 3, 18, 30)),
            # +12:45 -- a quarter-hour zone, which catches whole-hour arithmetic.
            ("day", "Pacific/Chatham", dt.datetime(2026, 7, 4, 11, 15)),
            ("year", "America/New_York", dt.datetime(2026, 1, 1, 5)),
            ("year", "Asia/Kolkata", dt.datetime(2025, 12, 31, 18, 30)),
            ("month", "America/New_York", dt.datetime(2026, 7, 1, 4)),
            ("quarter", "America/New_York", dt.datetime(2026, 7, 1, 4)),
            ("week", "America/New_York", dt.datetime(2026, 6, 28, 4)),
            ("hour", "Asia/Kolkata", dt.datetime(2026, 7, 4, 15, 30)),
            ("hour", "Pacific/Chatham", dt.datetime(2026, 7, 4, 15, 15)),
        ],
    )
    def test_boundaries_are_local(self, unit, tz, expected):
        assert trunc(unit, timezone=tz) == expected


class TestBinsCountFromTheYear2000:
    """Not from the epoch, and not from year 1 -- which is what it used to do."""

    def test_even_bin_lands_on_even_years(self):
        got = evaluate(
            {"$dateTrunc": {"date": dt.datetime(2021, 1, 1), "unit": "year", "binSize": 2}}, {}
        )
        assert got == dt.datetime(2020, 1, 1)

    def test_a_year_before_the_reference_floors_away_from_it(self):
        got = evaluate(
            {"$dateTrunc": {"date": dt.datetime(1999, 6, 1), "unit": "year", "binSize": 2}}, {}
        )
        assert got == dt.datetime(1998, 1, 1)

    def test_month_bins(self):
        assert trunc("month", binSize=5) == dt.datetime(2026, 4, 1)

    def test_week_defaults_to_sunday(self):
        """2026-07-04 is a Saturday; its week starts Sunday the 28th."""
        assert trunc("week") == dt.datetime(2026, 6, 28)

    def test_start_of_week_is_honoured(self):
        assert trunc("week", startOfWeek="monday") == dt.datetime(2026, 6, 29)
        assert trunc("week", startOfWeek="friday") == dt.datetime(2026, 7, 3)


class TestDstArithmetic:
    """Calendar units follow the wall clock; sub-day units follow real time."""

    TZ = "America/New_York"  # spring-forward 2026-03-08

    def test_day_bins_stay_on_local_midnight(self):
        for day, expected_hour in ((7, 5), (8, 5), (9, 4)):
            got = evaluate(
                {"$dateTrunc": {"date": "$d", "unit": "day", "timezone": self.TZ}},
                {"d": dt.datetime(2026, 3, day, 18)},
            )
            assert got == dt.datetime(2026, 3, day, expected_hour)

    @pytest.mark.parametrize(
        ("hour", "expected"),
        [
            (0, dt.datetime(2026, 3, 7, 22)),
            (6, dt.datetime(2026, 3, 8, 3)),
            (8, dt.datetime(2026, 3, 8, 8)),
            (12, dt.datetime(2026, 3, 8, 8)),
        ],
    )
    def test_hour_bins_stay_five_REAL_hours_apart(self, hour, expected):
        got = evaluate(
            {"$dateTrunc": {"date": "$d", "unit": "hour", "binSize": 5, "timezone": self.TZ}},
            {"d": dt.datetime(2026, 3, 8, hour, 30)},
        )
        assert got == expected


class TestDateDiffCountsBoundaries:
    def test_a_local_midnight_crossing_inside_one_utc_day(self):
        a, b = dt.datetime(2026, 7, 4, 2), dt.datetime(2026, 7, 4, 23)
        assert diff(a, b, "day", timezone="America/New_York") == 1
        assert diff(a, b, "day", timezone="UTC") == 0

    def test_not_whole_units_elapsed(self):
        """One month apart by boundary, zero by elapsed-whole-months."""
        a, b = dt.datetime(2026, 7, 1, 2), dt.datetime(2026, 7, 31, 23)
        assert diff(a, b, "month", timezone="America/New_York") == 1

    @pytest.mark.parametrize(
        ("a", "b", "expected"),
        [
            ((2, 30), (3, 10), 1),
            ((2, 0), (2, 59), 0),
            ((2, 59), (3, 0), 1),
        ],
    )
    def test_sub_day_units_count_boundaries_too(self, a, b, expected):
        start = dt.datetime(2026, 7, 4, *a)
        end = dt.datetime(2026, 7, 4, *b)
        assert diff(start, end, "hour") == expected

    def test_negative_direction(self):
        a, b = dt.datetime(2026, 7, 6), dt.datetime(2026, 7, 4)
        assert diff(a, b, "day") == -2


class TestUnrecognisedZones:
    @pytest.mark.parametrize("tz", ["Not/AZone", "", "  ", "Etc/GMT+99"])
    def test_are_rejected_with_40485(self, tz):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$hour": {"date": "$d", "timezone": tz}}, {"d": D})
        assert exc.value.code == 40485
        assert f'unrecognized time zone identifier: "{tz}"' in str(exc.value)

    def test_zone_names_are_case_sensitive(self):
        """`zoneinfo` resolves through the filesystem, so a case-insensitive one
        (macOS, Windows) used to accept this while mongod -- and Linux -- reject
        it, making the answer depend on the host."""
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$hour": {"date": "$d", "timezone": "America/new_york"}}, {"d": D})
        assert exc.value.code == 40485

    @pytest.mark.parametrize("op", ["$dateTrunc", "$dateDiff"])
    def test_two_operators_name_the_parameter(self, op):
        spec = (
            {"date": "$d", "unit": "day", "timezone": "Not/AZone"}
            if op == "$dateTrunc"
            else {"startDate": "$d", "endDate": "$d", "unit": "day", "timezone": "Not/AZone"}
        )
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: spec}, {"d": D})
        assert f"{op} parameter 'timezone' value parsing failed :: caused by ::" in str(exc.value)
