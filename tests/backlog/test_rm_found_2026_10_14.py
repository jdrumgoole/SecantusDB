"""Gap tests for Rust MongoDB server items found while the backlog was
verified on 2026-10-14 (measured against mongod 8.2.11)."""

from __future__ import annotations

from typing import Any

import bson
import pytest

from tests.backlog.conftest import mongo_answer


def _code(answer: tuple) -> tuple:
    return answer[:2] if answer[0] == "err" else answer


@pytest.mark.backlog("RM-139")
def test_date_from_parts_month_out_of_range_is_refused(rust_mongo: Any) -> None:
    """mongod 8.2.11 refuses a `month` outside [-32768, 32767] with 31034;
    the Rust server answered a date in the year 5350."""
    db = rust_mongo["rm_found"]
    pipeline = [
        {"$documents": [{}]},
        {"$project": {"_id": 0, "d": {"$dateFromParts": {"year": 2017, "month": 40000}}}},
    ]
    assert _code(mongo_answer(lambda: list(db.aggregate(pipeline)))) == ("err", 31034)


@pytest.mark.backlog("RM-140")
def test_profile_accepts_a_negative_slowms(rust_mongo: Any) -> None:
    """mongod 8.2.11 accepts `profile` with `slowms: -1` at levels 0 and 1
    and reads it back; the Rust server refused it (2)."""
    db = rust_mongo["rm_found"]
    got = mongo_answer(lambda: db.command("profile", 0, slowms=-1)["ok"])
    back = mongo_answer(lambda: db.command("profile", -1)["slowms"])
    assert (got, back) == (("ok", 1.0), ("ok", -1))


@pytest.mark.backlog("RM-141")
def test_sasl_start_for_an_unknown_user_fails(rust_mongo: Any) -> None:
    """mongod 8.2.11 fails `saslStart` for a user that does not exist at the
    first step: 18 `Authentication failed.`; the Rust server answered ok and
    continued the conversation."""
    payload = bson.Binary(b"n,,n=nobody_rm141,r=abcdefghijklmnopqrstuvwx")
    got = mongo_answer(
        lambda: rust_mongo.admin.command("saslStart", mechanism="SCRAM-SHA-256", payload=payload)[
            "ok"
        ]
    )
    assert got == ("err", 18, "Authentication failed.")
