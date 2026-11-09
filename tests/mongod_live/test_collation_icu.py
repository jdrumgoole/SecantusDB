"""Collation through ICU 57.1, on the Rust server and on mongod 8.2 (RM-091,
RM-095).

The Rust server links the ICU mongod links, from mongo's own copy and data,
and asks it what mongod asks: which locales and `@collation=` types exist,
what each option defaults to for a locale, how strings order. Each test runs
the same commands on both servers and compares the answers exactly.

Defaults are where the commands differ from one another on mongod: `create`
hands ICU the IDL-parsed collation with every option but `backwards` filled
in, so a locale's own defaults (`da` sorts capitals first, `yi` normalizes,
`fr_CA` compares accents backwards) show on an index, an explain and a
query's checks, and only `backwards` shows on a collection. Measured on
8.2.11, 2026-11-09.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same

#: Locales whose ICU defaults differ from the IDL's, and some that do not.
LOCALES = [
    "en",
    "da",
    "yi",
    "vi",
    "fr_CA",
    "th",
    "ja",
    "mt",
    "zh@collation=big5han",
    "zh@collation=gb2312han",
    "zh_Hant@collation=big5han",
    "ko@collation=search",
    "ko@collation=searchjl",
    "ln@collation=phonetic",
    "de@collation=search",
    "sv@collation=standard",
]

#: Locale strings mongod refuses, each with its own message.
REFUSED = [
    "EN",
    "en-US",
    "eng",
    "deu@collation=phonebook",
    "en_USA_POSIX",
    "iw",
    "und",
    "root",
    "xx",
    "en@collation=bogus",
    "en@colation=search",
    "zh_Hans",
    "ko_KR@collation=searchjl",
]

WORDS = [
    "a",
    "A",
    "á",
    "b",
    "中",
    "国",
    "人",
    "大",
    "一",
    "가",
    "각",
    "ㄱ",
    "ᄀ",
    "ɛ",
    "e",
    "ɔ",
    "o",
    "ọ̈",
    "ọ̈",
    "a b",
    "a\u200bb",
    "a-b",
    "ab",
]


def _db(client: Any, tag: str) -> Any:
    return client[f"collicu_{tag}_{uuid.uuid4().hex[:8]}"]


def _defaults(client: Any, locale: str) -> dict:
    db = _db(client, "defaults")
    try:
        db.t.insert_one({"s": "a"})
        out: dict = {}

        def index_collation() -> Any:
            db.t.create_index("s", name="ix", collation={"locale": locale})
            return next(i for i in db.t.list_indexes() if i["name"] == "ix")["collation"]

        def collection_collation() -> Any:
            db.create_collection("c", collation={"locale": locale})
            return next(db.list_collections(filter={"name": "c"}))["options"]["collation"]

        out["createIndexes"] = mongo_answer(index_collation)
        out["create"] = mongo_answer(collection_collation)
        out["explain"] = mongo_answer(
            lambda: db.command(
                "explain",
                {"find": "t", "filter": {"s": "a"}, "collation": {"locale": locale}},
                verbosity="queryPlanner",
            )["queryPlanner"].get("collation")
        )
        for strength in (1, 2):
            out[f"count {strength}"] = mongo_answer(
                lambda s=strength: db.t.count_documents(
                    {}, collation={"locale": locale, "strength": s}
                )
            )
            out[f"create {strength}"] = mongo_answer(
                lambda s=strength: (
                    db.create_collection(f"c{s}", collation={"locale": locale, "strength": s}).name
                )
            )
        return out
    finally:
        client.drop_database(db.name)


@pytest.mark.parametrize("locale", LOCALES)
def test_defaults_are_icus_where_mongod_asks_icu(rust_mongo: Any, mongod: Any, locale: str) -> None:
    same(
        {"locale": locale, **_defaults(rust_mongo, locale)},
        {"locale": locale, **_defaults(mongod, locale)},
    )


def _refusals(client: Any) -> list:
    db = _db(client, "refused")
    try:
        db.t.insert_one({"s": "a"})
        return [
            [loc, mongo_answer(lambda x=loc: db.t.count_documents({}, collation={"locale": x}))]
            for loc in REFUSED
        ]
    finally:
        client.drop_database(db.name)


def test_locales_are_refused_as_icu_reads_them(rust_mongo: Any, mongod: Any) -> None:
    same(_refusals(rust_mongo), _refusals(mongod))


def _order(client: Any, collation: dict) -> list:
    db = _db(client, "order")
    try:
        db.t.insert_many([{"_id": i, "s": w} for i, w in enumerate(WORDS)])
        return mongo_answer(
            lambda: [
                d["_id"] for d in db.t.find({}, collation=collation).sort([("s", 1), ("_id", 1)])
            ]
        )
    finally:
        client.drop_database(db.name)


@pytest.mark.parametrize(
    "collation",
    [
        {"locale": "en"},
        {"locale": "en", "normalization": True},
        {"locale": "en", "alternate": "shifted", "maxVariable": "space"},
        {"locale": "zh"},
        {"locale": "zh@collation=big5han"},
        {"locale": "zh@collation=gb2312han"},
        {"locale": "ko@collation=search"},
        {"locale": "ko@collation=searchjl"},
        {"locale": "ln@collation=phonetic"},
    ],
    ids=lambda c: ",".join(f"{k}={v}" for k, v in c.items()),
)
def test_order_is_icus(rust_mongo: Any, mongod: Any, collation: dict) -> None:
    same(_order(rust_mongo, collation), _order(mongod, collation))
