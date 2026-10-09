"""`sort: {$natural: ±1}` on the RUST server's `find` and `findAndModify`.

`$natural` in a `find` sort is storage order, forwards or backwards. It is not
a field path there, although the aggregation `$sort` stage reads it as one.
A shared sort validator added on 2026-10-05 applied the `$sort` rule to `find`
as well, so every `find().sort("$natural", ...)` answered 16410, and
`findAndModify` passed the spec down as a field name no document has, so
`{$natural: -1}` picked the FIRST document.

Every expectation below is mongod 8.2.11's own answer, measured 2026-10-09 by
running the same commands against both servers (38 cases, no differences).

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from bson import SON, Decimal128, Int64  # noqa: E402
from pymongo.errors import OperationFailure  # noqa: E402

NATURAL_VALUE = "$natural sort cannot be set to a value other than -1 or 1."


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_natural") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["natural"]
    d.c.drop()
    d.c.insert_many([{"_id": i, "a": 3 - i} for i in range(4)])
    d.c.create_index("a")
    try:
        yield d
    finally:
        cli.close()


def _find(db, sort, **extra):
    reply = db.command(SON([("find", "c"), ("sort", sort), *extra.items()]))
    return [d["_id"] for d in reply["cursor"]["firstBatch"]]


def _natural(value):
    return SON([("$natural", value)])


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        (1, [0, 1, 2, 3]),
        (-1, [3, 2, 1, 0]),
        (1.0, [0, 1, 2, 3]),
        (Int64(-1), [3, 2, 1, 0]),
        (Decimal128("-1.0"), [3, 2, 1, 0]),
    ],
)
def test_find_sorts_in_storage_order(db, value, expected) -> None:
    assert _find(db, _natural(value)) == expected


def test_pymongo_cursor_sort_natural(db) -> None:
    """The shape `pymongo`'s own suite uses to read the newest oplog entry."""
    assert [d["_id"] for d in db.c.find().sort("$natural", pymongo.DESCENDING)] == [3, 2, 1, 0]
    assert db.c.find().sort("$natural", pymongo.DESCENDING).limit(-1).next()["_id"] == 3


def test_natural_sort_composes_with_filter_skip_and_limit(db) -> None:
    assert _find(db, _natural(-1), filter={"a": {"$gte": 2}}) == [1, 0]
    assert _find(db, _natural(-1), skip=1, limit=2) == [2, 1]


@pytest.mark.parametrize(
    "sort",
    [
        _natural(2),
        _natural(0),
        _natural(1.9),
        _natural(Decimal128("1.4")),
        _natural(True),
        _natural(None),
        _natural("x"),
        SON([("$natural", 1), ("a", 1)]),
        SON([("a", 1), ("$natural", -1)]),
    ],
)
def test_anything_but_a_lone_plus_or_minus_one_is_refused(db, sort) -> None:
    with pytest.raises(OperationFailure) as err:
        _find(db, sort)
    assert (err.value.code, err.value.details["errmsg"]) == (2, NATURAL_VALUE)


def test_hint_must_agree_with_a_natural_sort(db) -> None:
    assert _find(db, _natural(-1), hint=_natural(-1)) == [3, 2, 1, 0]
    with pytest.raises(OperationFailure) as err:
        _find(db, _natural(-1), hint=_natural(1))
    assert err.value.details["errmsg"] == (
        "$natural hint must be in the same direction as $natural sort order"
    )
    for hint in ({"a": 1}, "a_1"):
        with pytest.raises(OperationFailure) as err:
            _find(db, _natural(1), hint=hint)
        assert (err.value.code, err.value.details["errmsg"]) == (
            2,
            "index hint not allowed with $natural sort order",
        )


def test_other_dollar_names_and_the_sort_stage_still_answer_16410(db) -> None:
    for sort in (SON([("$other", 1)]), SON([("x.$natural", 1)])):
        with pytest.raises(OperationFailure) as err:
            _find(db, sort)
        assert err.value.code == 16410
    with pytest.raises(OperationFailure) as err:
        list(db.c.aggregate([{"$sort": _natural(1)}]))
    assert err.value.code == 16410


def test_find_and_modify_takes_the_last_document(db) -> None:
    removed = db.command(SON([("findAndModify", "c"), ("sort", _natural(-1)), ("remove", True)]))
    assert removed["value"] == {"_id": 3, "a": 0}
    assert db.c.count_documents({}) == 3
    with pytest.raises(OperationFailure) as err:
        db.command(SON([("findAndModify", "c"), ("sort", _natural(2)), ("remove", True)]))
    assert (err.value.code, err.value.details["errmsg"]) == (2, NATURAL_VALUE)


def test_explain_reports_a_backward_collection_scan(db) -> None:
    plan = db.command(
        SON(
            [
                ("explain", SON([("find", "c"), ("sort", _natural(-1))])),
                ("verbosity", "queryPlanner"),
            ]
        )
    )["queryPlanner"]["winningPlan"]
    assert (plan["stage"], plan["direction"]) == ("COLLSCAN", "backward")
