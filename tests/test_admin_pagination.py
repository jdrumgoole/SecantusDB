"""Skip-ID pagination helpers + paged_collection facade method."""

from __future__ import annotations

import base64
import json
import uuid

import pytest
from bson import ObjectId
from bson.binary import Binary
from bson.decimal128 import Decimal128

from secantus.admin.pagination import (
    PageCursor,
    build_page_filter,
    decode_cursor,
    detect_id_type,
    encode_cursor,
    make_next_cursor,
)

# ---- detect_id_type --------------------------------------------------------


def test_detect_id_type_objectid() -> None:
    assert detect_id_type(ObjectId()) == "oid"


def test_detect_id_type_int() -> None:
    assert detect_id_type(42) == "int"


def test_detect_id_type_str() -> None:
    assert detect_id_type("user-1") == "str"


def test_detect_id_type_rejects_bool() -> None:
    with pytest.raises(ValueError):
        detect_id_type(True)


def test_detect_id_type_rejects_float() -> None:
    with pytest.raises(ValueError):
        detect_id_type(3.14)


def test_detect_id_type_decimal128() -> None:
    assert detect_id_type(Decimal128("1.50")) == "dec"


def test_detect_id_type_uuid() -> None:
    assert detect_id_type(uuid.uuid4()) == "uuid"


def test_detect_id_type_binary() -> None:
    assert detect_id_type(Binary(b"\x00\x01\xff", 0)) == "bin"


def test_detect_id_type_rejects_document() -> None:
    with pytest.raises(ValueError):
        detect_id_type({"a": 1})


# ---- encode/decode round-trip ---------------------------------------------


def test_roundtrip_decimal128_preserves_exact_form() -> None:
    # "1.50" must not collapse to "1.5" — the cursor has to compare equal
    # to the stored value, and Decimal128 keeps coefficient + exponent.
    original = Decimal128("1.50")
    token = encode_cursor(PageCursor(after=original, type_tag="dec"))
    restored = decode_cursor(token)
    assert restored is not None
    assert str(restored.after) == "1.50"
    assert restored.after == original


def test_roundtrip_uuid() -> None:
    original = uuid.uuid4()
    token = encode_cursor(PageCursor(after=original, type_tag="uuid"))
    restored = decode_cursor(token)
    assert restored is not None
    assert restored.after == original


@pytest.mark.parametrize("subtype", [0, 3, 4])
def test_roundtrip_binary_preserves_subtype(subtype: int) -> None:
    original = Binary(b"\x00\x01\xff\xfe", subtype)
    token = encode_cursor(PageCursor(after=original, type_tag="bin"))
    restored = decode_cursor(token)
    assert restored is not None
    assert restored.after == original
    assert restored.after.subtype == subtype


def _forge_token(after: str, type_tag: str) -> str:
    """Build a cursor token directly, bypassing _serialize.

    Lets a test hand-craft the tampered tokens a user could produce by
    editing ``?after=`` in the URL bar.
    """
    raw = json.dumps({"after": after, "type": type_tag}, separators=(",", ":"))
    return base64.urlsafe_b64encode(raw.encode("utf-8")).decode("ascii").rstrip("=")


@pytest.mark.parametrize(
    ("after", "type_tag"),
    [
        # bson raises InvalidId, not ValueError.
        ("not-an-oid", "oid"),
        # decimal raises InvalidOperation, not ValueError.
        ("garbage", "dec"),
        ("not-a-uuid", "uuid"),
        # binary payloads carry a "<subtype-hex>:<base64>" shape.
        ("no-subtype-separator", "bin"),
        ("zz:!!!!", "bin"),
        ("00:!!!!not-base64", "bin"),
        ("nope", "no_such_type"),
    ],
)
def test_decode_tampered_token_raises_value_error(after: str, type_tag: str) -> None:
    # A hand-edited ?after= must surface as a 400-shaped ValueError, never
    # an uncaught 500 from a bson constructor.
    with pytest.raises(ValueError):
        decode_cursor(_forge_token(after, type_tag))


@pytest.mark.parametrize(
    "value,type_tag",
    [
        (ObjectId("507f1f77bcf86cd799439011"), "oid"),
        (12345, "int"),
        (-7, "int"),
        ("user-42", "str"),
        ("with spaces and / slashes + plus", "str"),
    ],
)
def test_cursor_round_trip(value: object, type_tag: str) -> None:
    token = encode_cursor(PageCursor(after=value, type_tag=type_tag))
    decoded = decode_cursor(token)
    assert decoded is not None
    assert decoded.type_tag == type_tag
    assert decoded.after == value


def test_decode_none_returns_none() -> None:
    assert decode_cursor(None) is None
    assert decode_cursor("") is None


def test_decode_malformed_raises() -> None:
    with pytest.raises(ValueError):
        decode_cursor("not-valid-base64-😀")
    with pytest.raises(ValueError):
        decode_cursor("YWJj")  # valid b64 but not JSON


def test_decode_unknown_type_tag_raises() -> None:
    import base64
    import json

    payload = json.dumps({"after": "x", "type": "weird"}).encode()
    token = base64.urlsafe_b64encode(payload).decode().rstrip("=")
    with pytest.raises(ValueError):
        decode_cursor(token)


# ---- build_page_filter ----------------------------------------------------


def test_build_page_filter_no_cursor_returns_filter_copy() -> None:
    base = {"status": "active"}
    out = build_page_filter(base, cursor=None, sort_dir=1)
    assert out == {"status": "active"}
    out["status"] = "x"
    assert base["status"] == "active"  # copy, not the same dict


def test_build_page_filter_adds_gt_when_ascending() -> None:
    cursor = PageCursor(after=10, type_tag="int")
    out = build_page_filter({"x": 1}, cursor=cursor, sort_dir=1)
    assert out == {"x": 1, "_id": {"$gt": 10}}


def test_build_page_filter_adds_lt_when_descending() -> None:
    cursor = PageCursor(after=10, type_tag="int")
    out = build_page_filter(None, cursor=cursor, sort_dir=-1)
    assert out == {"_id": {"$lt": 10}}


def test_build_page_filter_rejects_id_in_filter() -> None:
    with pytest.raises(ValueError):
        build_page_filter({"_id": 5}, cursor=None, sort_dir=1)


def test_build_page_filter_rejects_bad_sort_dir() -> None:
    with pytest.raises(ValueError):
        build_page_filter(None, cursor=None, sort_dir=0)


# ---- make_next_cursor ------------------------------------------------------


def test_make_next_cursor_returns_none_when_exhausted() -> None:
    rows = [{"_id": ObjectId()}, {"_id": ObjectId()}]
    assert make_next_cursor(rows, page_size=5) is None


def test_make_next_cursor_emits_token_when_overfetched() -> None:
    rows = [{"_id": i} for i in range(5)]
    token = make_next_cursor(rows, page_size=4)
    assert token is not None
    decoded = decode_cursor(token)
    assert decoded is not None
    assert decoded.after == 3  # last id in the page (index page_size-1)
    assert decoded.type_tag == "int"


# ---- paged_collection (integration with in-process server) ----------------


@pytest.fixture
def server(tmp_path):
    from secantus import SecantusDBServer

    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def facade(server):
    from secantus.admin.client import MongoFacade

    f = MongoFacade(server.uri)
    yield f
    f.close()


def test_paged_collection_walks_full_collection(server, facade) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["paged_db"]["c"]
        coll.insert_many([{"_id": i, "x": i} for i in range(25)])
    finally:
        mc.close()

    seen: list[int] = []
    cursor = None
    for _ in range(10):  # safety bound
        rows, token = facade.paged_collection("paged_db", "c", cursor=cursor, page_size=10)
        seen.extend(d["_id"] for d in rows)
        if token is None:
            break
        cursor = decode_cursor(token)
    assert seen == list(range(25))


def test_paged_collection_descending(server, facade) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["desc_db"]["c"].insert_many([{"_id": i} for i in range(8)])
    finally:
        mc.close()

    seen: list[int] = []
    cursor = None
    for _ in range(5):
        rows, token = facade.paged_collection(
            "desc_db", "c", sort_dir=-1, cursor=cursor, page_size=3
        )
        seen.extend(d["_id"] for d in rows)
        if token is None:
            break
        cursor = decode_cursor(token)
    assert seen == [7, 6, 5, 4, 3, 2, 1, 0]


def test_paged_collection_with_filter(server, facade) -> None:
    from pymongo import MongoClient

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        mc["filter_db"]["c"].insert_many([{"_id": i, "active": i % 2 == 0} for i in range(20)])
    finally:
        mc.close()

    rows, token = facade.paged_collection(
        "filter_db", "c", filter_doc={"active": True}, page_size=100
    )
    assert [d["_id"] for d in rows] == [0, 2, 4, 6, 8, 10, 12, 14, 16, 18]
    assert token is None  # all 10 fit


def test_paged_collection_empty_collection(server, facade) -> None:
    rows, token = facade.paged_collection("empty_db", "missing_coll", page_size=5)
    assert rows == []
    assert token is None
