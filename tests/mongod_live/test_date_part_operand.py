"""A date-part operator (`$year` ... `$isoDayOfWeek`) given its operand as a
literal array of one takes that one element as the operand, as mongod's
`DateExpressionAcceptingTimeZone::parse` does: a field path in it is null
when missing, and a `{date: ...}` in it is an object expression, not the
options form. An array VALUE is never unwrapped -- `{$year: "$a"}` with
`a: [date]` is 16006, as is `$dateToString` of one. The Rust server
evaluated the literal array and unwrapped the value instead (RM-533,
measured 8.2.11 on 2026-11-14)."""

from __future__ import annotations

import datetime
from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same

D = datetime.datetime(2020, 1, 9)

EXPRESSIONS: list[Any] = [
    {"$isoWeek": ["$t"]},
    {"$year": ["$t"]},
    {"$month": ["$missing"]},
    {"$week": [None]},
    {"$year": [D]},
    {"$year": [{"$add": ["$t", 1]}]},
    {"$isoWeek": [{"date": "$t"}]},
    {"$year": [[D]]},
    {"$year": [["$t"]]},
    {"$year": "$a"},
    {"$year": "$a2"},
    {"$year": {"date": "$a"}},
    {"$year": {"date": ["$t"]}},
    {"$hour": ["$t", "$t"]},
    {"$year": []},
    {"$dateToString": {"date": "$a"}},
    {"$dateToString": {"date": ["$t"]}},
    {"$toDate": "$a"},
]


@pytest.mark.parametrize("expr", EXPRESSIONS, ids=[str(e) for e in EXPRESSIONS])
def test_a_date_operand_array_is_unwrapped_as_mongod_unwraps_it(
    rust_mongo: Any, mongod: Any, expr: Any
) -> None:
    def run(client: Any) -> tuple:
        coll = client.date_part_operand.c
        coll.drop()
        coll.insert_many([{"_id": 0}, {"_id": 1, "t": D, "a": [D], "a2": [D, D]}])
        return mongo_answer(lambda: list(coll.aggregate([{"$project": {"r": expr}}])))

    same(run(rust_mongo), run(mongod))
