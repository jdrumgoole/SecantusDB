"""`$toDate` accepts timelib's format table, not just ISO-8601.

mongod's `$toDate` runs timelib. Both servers implemented a small ISO subset and
rejected the rest, so `{$toDate: "12/31/2020"}` — an ordinary call — answered
`241` where mongod returns a date. 15 of 19 shapes diverged when this was first
measured (8.2.11, 2026-09-09).

The REJECTIONS below are as load-bearing as the acceptances: a parser that takes
too much is as wrong as one that takes too little, and only the rejections catch
it. Wider sweep: `tools/probes/todate_string_formats.py`.
"""

from __future__ import annotations

import datetime as dt

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    path = tmp_path_factory.mktemp("todatefmt")
    server = SecantusDBServer(port=0, storage_path=str(path / "store"))
    server.start()
    host, port = server.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        yield client["todatefmt"]["c"]
    finally:
        client.close()
        server.stop()


def _to_date(coll, text):
    coll.delete_many({})
    coll.insert_one({"_id": 1, "s": text})
    return list(coll.aggregate([{"$project": {"v": {"$toDate": "$s"}}}]))[0]["v"]


JAN1 = dt.datetime(2020, 1, 1)
DEC31 = dt.datetime(2020, 12, 31)


@pytest.mark.parametrize(
    "text,expected",
    [
        # The US slash form -- month FIRST, padded or not, with an optional time.
        ("12/31/2020", DEC31),
        ("1/2/2020", dt.datetime(2020, 1, 2)),
        ("01/02/2020", dt.datetime(2020, 1, 2)),
        ("12/31/2020 10:30", dt.datetime(2020, 12, 31, 10, 30)),
        # Year-first slash, disambiguated by the four-digit leading field.
        ("2020/12/31", DEC31),
        ("2020/1/2", dt.datetime(2020, 1, 2)),
        # Non-padded ISO.
        ("2020-1-1", JAN1),
        ("2020-1-1 10:30", dt.datetime(2020, 1, 1, 10, 30)),
        # Month names, either order, either case, optional comma.
        ("Dec 31 2020", DEC31),
        ("dec 31 2020", DEC31),
        ("DEC 31 2020", DEC31),
        ("December 31 2020", DEC31),
        ("31 Dec 2020", DEC31),
        ("31 December 2020", DEC31),
        ("Dec 31, 2020", DEC31),
        # `@<unix seconds>`, negative and fractional included.
        ("@1577836800", JAN1),
        ("@0", dt.datetime(1970, 1, 1)),
        ("@-1", dt.datetime(1969, 12, 31, 23, 59, 59)),
        ("@1577836800.5", dt.datetime(2020, 1, 1, 0, 0, 0, 500000)),
        # Compact and week forms, and an hour with no minutes.
        ("20200101", JAN1),
        ("20200101T120000", dt.datetime(2020, 1, 1, 12)),
        ("2020-W01-1", dt.datetime(2019, 12, 30)),
        ("2020-01-01T00", JAN1),
        # Surrounding whitespace is tolerated.
        ("2020-01-01 ", JAN1),
        ("  2020-01-01", JAN1),
        ("2020-01-01\t", JAN1),
    ],
)
def test_timelib_accepts_these(coll, text, expected):
    assert _to_date(coll, text) == expected


@pytest.mark.parametrize(
    "letter,hours",
    [
        ("A", 1),
        ("B", 2),
        ("I", 9),
        ("K", 10),
        ("M", 12),
        ("N", -1),
        ("T", -7),
        ("Y", -12),
        ("Z", 0),
    ],
)
def test_a_trailing_letter_is_a_military_timezone(coll, letter, hours):
    """`2020-01-01T` is 07:00:00, NOT midnight: the trailing `T` is the zone
    UTC-7, not the ISO date/time separator.

    Deterministic and not host-local -- a `TZ=UTC` mongod answers the same
    (measured 2026-09-09).
    """
    assert _to_date(coll, f"2020-01-01{letter}") == JAN1 - dt.timedelta(hours=hours)


def test_the_lowercase_form_is_the_same_zone(coll):
    assert _to_date(coll, "2020-01-01t") == _to_date(coll, "2020-01-01T")


@pytest.mark.parametrize(
    "text",
    [
        # `J` is the one military letter timelib rejects.
        "2020-01-01J",
        # The slash form is US-first BY RULE -- day-first is refused outright,
        # so this is not ambiguity-resolution.
        "31/12/2020",
        # An out-of-range component is a parse failure, not a rollover.
        "13/01/2020",
        "12/32/2020",
        "2020-02-30",
        # Still not dates.
        "not a date",
        "abc",
        "2020",
        "   ",
        "",
    ],
)
def test_these_are_still_refused(coll, text):
    coll.delete_many({})
    coll.insert_one({"_id": 1, "s": text})
    with pytest.raises(pymongo.errors.OperationFailure) as excinfo:
        list(coll.aggregate([{"$project": {"v": {"$toDate": "$s"}}}]))
    assert excinfo.value.code == 241
