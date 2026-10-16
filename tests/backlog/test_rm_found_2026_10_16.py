"""Gap tests for Rust MongoDB server items filed during Phase 5 on
2026-10-16 (measured against mongod 8.2.11): the 7.078 notes that were
not yet items, and `showRecordId` beside RM-083.
"""

from __future__ import annotations

import datetime
import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _db(client: Any, prefix: str) -> Any:
    return client[f"{prefix}_{uuid.uuid4().hex[:8]}"]


def _metrics_keys(client: Any) -> list:
    db = _db(client, "rm170")
    db.c.insert_many([{"_id": i} for i in range(3)])
    found = db.command("find", "c", includeQueryStatsMetrics=True)["cursor"]
    aggregated = db.command(
        "aggregate", "c", pipeline=[], cursor={}, includeQueryStatsMetrics=True
    )["cursor"]
    distinct = db.command("distinct", "c", key="_id", includeQueryStatsMetrics=True)
    return [sorted(r.get("metrics", {})) for r in (found, aggregated, distinct)]


@pytest.mark.backlog("RM-170")
def test_include_query_stats_metrics_adds_a_metrics_document(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `includeQueryStatsMetrics: true` on `find`,
    `aggregate` and `distinct` with a `metrics` document (`keysExamined`,
    `docsExamined`, `cpuNanos`, ...). Its fields are compared, not their
    values, which count work and time."""
    expected = _metrics_keys(mongod)
    assert all(expected)
    assert _metrics_keys(rust_mongo) == expected


def _distinct_multikey(client: Any) -> Any:
    db = _db(client, "rm171")
    db.m.insert_many([{"_id": 1, "a": [8, [9]]}, {"_id": 2, "a": 7}])
    db.m.create_index("a")
    return mongo_answer(lambda: db.command("distinct", "m", key="a")["values"])


@pytest.mark.backlog("RM-171")
def test_distinct_over_a_multikey_index(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers a `distinct` with no query over a multikey index
    from a DISTINCT_SCAN of the index keys, so a value nested as `[8, [9]]`
    gives `9` (its key `[9]`, unwound): `[7, 8, 9]`. Without the index it is
    `[7, 8, [9]]`."""
    expected = _distinct_multikey(mongod)
    assert expected == ("ok", [7, 8, 9])
    assert _distinct_multikey(rust_mongo) == expected


def _distinct_collated(client: Any) -> Any:
    db = _db(client, "rm172")
    db.c.insert_many(
        [
            {"_id": 1, "s": "b"},
            {"_id": 2, "s": "B"},
            {"_id": 3},
            {"_id": 4, "s": None},
            {"_id": 5, "s": "a"},
        ]
    )
    collation = {"locale": "en", "strength": 2}
    db.c.create_index("s", name="s_en", collation=collation)
    return mongo_answer(lambda: db.command("distinct", "c", key="s", collation=collation)["values"])


@pytest.mark.backlog("RM-172")
def test_distinct_through_a_collated_index_leaves_out_null(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers a `distinct` under the collation of an index on
    the key from a fetching DISTINCT_SCAN: one document per index key, the
    first in index order. The null key's first document lacks the field, so
    `null` is left out: `["a", "b"]`."""
    expected = _distinct_collated(mongod)
    assert expected == ("ok", ["a", "b"])
    assert _distinct_collated(rust_mongo) == expected


def _show_record_id(client: Any) -> Any:
    db = _db(client, "rm083")
    db.c.insert_many([{"_id": i} for i in range(1, 5)])
    db.c.delete_one({"_id": 1})
    return [d.get("$recordId") for d in db.c.find({}, show_record_id=True)]


@pytest.mark.backlog("RM-083")
def test_show_record_id_is_the_record_id(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `showRecordId` is each document's RecordId -- after
    the first of four is deleted, 2, 3, 4 -- not its position in the reply."""
    expected = _show_record_id(mongod)
    assert expected == [2, 3, 4]
    assert _show_record_id(rust_mongo) == expected


_TZ_DATE = datetime.datetime(2024, 3, 10, 6, 30)

_TZ_OPERATORS = {
    "hour": {"$hour": {"date": "$d", "timezone": "$tz"}},
    "dateToString": {"$dateToString": {"date": "$d", "timezone": "$tz"}},
    "dateToParts": {"$dateToParts": {"date": "$d", "timezone": "$tz"}},
    "dateFromParts": {"$dateFromParts": {"year": 2024, "hour": 6, "timezone": "$tz"}},
    "dateAdd": {"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 1, "timezone": "$tz"}},
    "dateTrunc": {"$dateTrunc": {"date": "$d", "unit": "day", "timezone": "$tz"}},
}


def _tz_answers(client: Any, zones: list) -> list:
    coll = _db(client, "rm173").c
    coll.insert_many([{"_id": i, "d": _TZ_DATE, "tz": z} for i, z in enumerate(zones)])
    out = []
    for name, op in _TZ_OPERATORS.items():
        for i in range(len(zones)):
            pipeline = [{"$match": {"_id": i}}, {"$project": {"_id": 0, "r": op}}]
            got = mongo_answer(lambda p=pipeline: list(coll.aggregate(p)))
            out.append((name, zones[i], got[:2] if got[0] == "err" else got))
    return out


@pytest.mark.backlog_fixed("RM-173")
def test_a_date_operators_timezone_is_an_expression(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 evaluates `timezone`: `"$tz"` reads the zone from the
    document, a null one answers null, a non-string is 40517. The Rust server
    read `"$tz"` as the zone's NAME and failed 40485 on every document."""
    zones = ["America/New_York", "+05:00", None, 5, "Bad/Zone"]
    assert _tz_answers(rust_mongo, zones) == _tz_answers(mongod, zones)


@pytest.mark.backlog_fixed("RM-173")
def test_fixed_offsets_mongod_accepts(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 takes `+HH` and offsets past 23:59 or with 60 minutes
    as zones (`+05`, `-05`, `+24:00`, `+05:60`); the Rust server's extractors
    and `$dateToString` refused them as unknown (40485)."""
    zones = ["+05", "-05", "+24:00", "+05:60"]
    assert _tz_answers(rust_mongo, zones) == _tz_answers(mongod, zones)


@pytest.mark.backlog_fixed("RM-173")
def test_date_to_string_default_format_drops_z_outside_utc(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's default `$dateToString` format ends in `Z` only for
    UTC itself (no zone, `UTC`, a zero offset); under `GMT` or a named zone
    it does not."""

    def run(client: Any) -> list:
        out = []
        for tz in ["UTC", "+00:00", "GMT", "Europe/London", "+05:00"]:
            pipeline = [
                {"$documents": [{}]},
                {
                    "$project": {
                        "_id": 0,
                        "r": {"$dateToString": {"date": _TZ_DATE, "timezone": tz}},
                    }
                },
            ]
            out.append(mongo_answer(lambda p=pipeline: list(client["rm173"].aggregate(p))))
        return out

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-126")
@pytest.mark.parametrize(
    "spec",
    [
        {"year": 2017, "isoWeek": 3},
        {"isoWeekYear": 2017, "month": 3},
        {"year": 2017, "day": 40000},
        {"year": 2017, "hour": -32769},
        {"isoWeekYear": 0},
        {"year": 2017, "month": "x", "timezone": "Bad/Zone"},
        {"year": 2017, "month": None, "hour": "x"},
    ],
    ids=[
        "mixed-natural",
        "mixed-iso",
        "day-range",
        "hour-range",
        "iso-year-range",
        "zone-first",
        "hour-first",
    ],
)
def test_date_from_parts_errors_and_their_order(rust_mongo: Any, mongod: Any, spec: dict) -> None:
    """mongod 8.2.11: 40489, 40525, 31034, 31034, 31095, 40485 and 40515 --
    the time of day is read before the zone, and the zone before the date."""
    pipeline = [{"$documents": [{}]}, {"$project": {"_id": 0, "d": {"$dateFromParts": spec}}}]
    answers = [
        mongo_answer(lambda c=c: list(c["rm126"].aggregate(pipeline))) for c in (rust_mongo, mongod)
    ]
    assert answers[0] == answers[1]


def _sasl_start_nonces(client: Any) -> list:
    from bson.binary import Binary

    admin = client["admin"]
    admin.command("createUser", "rm174", pwd="pw", roles=[])
    try:
        out = []
        for nonce in ["abc", "a,b", "a=b", "abcd"]:
            r = admin.command(
                "saslStart",
                1,
                mechanism="SCRAM-SHA-256",
                payload=Binary(f"n,,n=rm174,r={nonce}".encode()),
                autoAuthorize=1,
                check=False,
            )
            out.append((nonce, r.get("ok"), r.get("code"), r.get("errmsg")))
        return out
    finally:
        admin.command("dropUser", "rm174")


@pytest.mark.backlog_fixed("RM-174")
def test_sasl_start_refuses_a_client_nonce_under_four_characters(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 fails `saslStart` with 18 `Authentication failed.` when
    the client nonce is shorter than four characters (`a,b` is the nonce
    `a`); the Rust server started the conversation."""
    assert _sasl_start_nonces(rust_mongo) == _sasl_start_nonces(mongod)
