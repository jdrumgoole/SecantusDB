"""MongoDB 8.0's ``bulkWrite``, and ``sort`` on an update statement.

Every expectation here was probed against a live mongod 8.2.11 on 2026-08-30
(``tools/probes/`` style: run the same command against both servers, compare).
The two features and the advertised version move together on purpose -- the
driver spec suites gate on the version in BOTH directions, asserting that a
pre-8.0 server REJECTS ``sort`` and that an 8.0 one honours it -- so a bump
without the features, or features without the bump, fails one side or the other.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from pymongo import DeleteOne, InsertOne, MongoClient, UpdateOne

from secantus import SecantusDBServer, commands


@pytest.fixture
def server(tmp_path) -> Iterator[SecantusDBServer]:
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "wt"))
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def client(server: SecantusDBServer) -> Iterator[MongoClient]:
    cli = MongoClient(server.address[0], server.address[1], directConnection=True)
    try:
        yield cli
    finally:
        cli.close()


def _seed(client: MongoClient) -> list[dict]:
    client.drop_database("bwa")
    client.bwa.c.insert_many([{"_id": i, "v": i} for i in (1, 2, 3)])
    return [{"ns": "bwa.c"}, {"ns": "bwb.d"}]


def test_advertises_an_8_x_server(client: MongoClient) -> None:
    """The capability contract the two features below rely on."""
    assert client.admin.command("hello")["maxWireVersion"] == commands.WIRE_VERSION >= 25
    assert client.admin.command("buildInfo")["version"].startswith("8.")


def test_bulk_write_across_namespaces(client: MongoClient) -> None:
    ns = _seed(client)
    reply = client.admin.command(
        {
            "bulkWrite": 1,
            "ops": [
                {"insert": 0, "document": {"_id": 10, "v": 10}},
                {
                    "update": 0,
                    "filter": {"_id": 1},
                    "updateMods": {"$set": {"v": 99}},
                    "multi": False,
                },
                {"delete": 0, "filter": {"_id": 2}, "multi": False},
                {"insert": 1, "document": {"_id": 20}},
            ],
            "nsInfo": ns,
        }
    )
    assert reply["cursor"]["ns"] == "admin.$cmd.bulkWrite"
    assert reply["cursor"]["id"] == 0
    assert reply["cursor"]["firstBatch"] == [
        {"ok": 1.0, "idx": 0, "n": 1},
        {"ok": 1.0, "idx": 1, "n": 1, "nModified": 1},
        {"ok": 1.0, "idx": 2, "n": 1},
        {"ok": 1.0, "idx": 3, "n": 1},
    ]
    assert (reply["nInserted"], reply["nMatched"], reply["nModified"]) == (2, 1, 1)
    assert (reply["nDeleted"], reply["nUpserted"], reply["nErrors"]) == (1, 0, 0)
    # The writes really landed, in both databases.
    assert sorted(d["_id"] for d in client.bwa.c.find()) == [1, 3, 10]
    assert client.bwb.d.find_one()["_id"] == 20


@pytest.mark.parametrize(
    ("ordered", "n_inserted", "batch_len"),
    [(True, 0, 1), (False, 1, 2)],
    ids=["ordered-stops", "unordered-continues"],
)
def test_bulk_write_ordering_on_error(
    client: MongoClient, ordered: bool, n_inserted: int, batch_len: int
) -> None:
    """An ordered batch stops at the first error; an unordered one carries on."""
    ns = _seed(client)
    reply = client.admin.command(
        {
            "bulkWrite": 1,
            "ops": [
                {"insert": 0, "document": {"_id": 1}},  # duplicate
                {"insert": 0, "document": {"_id": 9}},
            ],
            "nsInfo": ns,
            "ordered": ordered,
        }
    )
    assert reply["nErrors"] == 1
    assert reply["nInserted"] == n_inserted
    batch = reply["cursor"]["firstBatch"]
    assert len(batch) == batch_len
    assert batch[0]["ok"] == 0.0
    assert batch[0]["code"] == 11000
    assert batch[0]["keyValue"] == {"_id": 1}
    assert batch[0]["n"] == 0


def test_bulk_write_upsert_reports_the_id(client: MongoClient) -> None:
    ns = _seed(client)
    reply = client.admin.command(
        {
            "bulkWrite": 1,
            "ops": [
                {
                    "update": 0,
                    "filter": {"_id": 77},
                    "updateMods": {"$set": {"v": 7}},
                    "upsert": True,
                    "multi": False,
                }
            ],
            "nsInfo": ns,
        }
    )
    assert reply["nUpserted"] == 1
    assert reply["nMatched"] == 0
    assert reply["cursor"]["firstBatch"][0]["upserted"] == {"_id": 77}


def test_errors_only_suppresses_successful_results(client: MongoClient) -> None:
    ns = _seed(client)
    reply = client.admin.command(
        {
            "bulkWrite": 1,
            "ops": [{"insert": 0, "document": {"_id": 50}}],
            "nsInfo": ns,
            "errorsOnly": True,
        }
    )
    assert reply["nInserted"] == 1
    assert reply["cursor"]["firstBatch"] == []


@pytest.mark.parametrize(
    ("cmd_extra", "code", "fragment"),
    [
        ({"ops": []}, 16, "Write batch sizes must be between 1 and 100000"),
        ({"ops": [{"insert": 5, "document": {"x": 1}}]}, 2, "invalid nsInfo index"),
        ({"ops": [{"insert": 0, "document": {"x": 1}}], "bogus": 1}, 40415, "unknown field"),
        ({"ops": [{"insert": 0}]}, 40414, "is missing but a required field"),
    ],
    ids=["empty-ops", "bad-ns-index", "unknown-field", "missing-document"],
)
def test_bulk_write_rejections(
    client: MongoClient, cmd_extra: dict, code: int, fragment: str
) -> None:
    ns = _seed(client)
    with pytest.raises(Exception) as ei:  # noqa: PT011 - the code is the assertion
        client.admin.command({"bulkWrite": 1, "nsInfo": ns, **cmd_extra})
    details = ei.value.details or {}
    assert details.get("code") == code
    assert fragment in details.get("errmsg", "")


def test_bulk_write_requires_the_admin_database(client: MongoClient) -> None:
    ns = _seed(client)
    with pytest.raises(Exception) as ei:  # noqa: PT011
        client.bwa.command(
            {"bulkWrite": 1, "ops": [{"insert": 0, "document": {"x": 1}}], "nsInfo": ns}
        )
    details = ei.value.details or {}
    assert details.get("code") == 13
    assert details["errmsg"] == "bulkWrite may only be run against the admin database."


def test_client_level_bulk_write_through_the_driver(client: MongoClient) -> None:
    """The API the version bump exists to enable."""
    _seed(client)
    res = client.bulk_write(
        [
            InsertOne({"_id": 5, "v": 5}, namespace="bwa.c"),
            UpdateOne({"_id": 1}, {"$set": {"v": 99}}, namespace="bwa.c"),
            DeleteOne({"_id": 2}, namespace="bwa.c"),
        ]
    )
    assert (res.inserted_count, res.matched_count, res.modified_count, res.deleted_count) == (
        1,
        1,
        1,
        1,
    )


# --- `sort` on an update statement (also MongoDB 8.0) ------------------------


def test_update_sort_picks_the_first_in_sort_order(client: MongoClient) -> None:
    client.drop_database("srt")
    coll = client.srt.c
    coll.insert_many([{"_id": 1, "v": 3}, {"_id": 2, "v": 1}, {"_id": 3, "v": 2}])
    coll.update_one({}, {"$set": {"hit": 1}}, sort={"v": 1})
    assert coll.find_one({"hit": 1})["_id"] == 2

    coll.update_one({}, {"$set": {"hit2": 1}}, sort={"v": -1})
    assert coll.find_one({"hit2": 1})["_id"] == 1


def test_update_sort_with_multi_is_rejected(client: MongoClient) -> None:
    """mongod refuses the combination -- probed 8.2.11."""
    client.drop_database("srt")
    client.srt.c.insert_one({"_id": 1, "v": 1})
    with pytest.raises(Exception) as ei:  # noqa: PT011 - the code is the assertion
        client.srt.command(
            {
                "update": "c",
                "updates": [{"q": {}, "u": {"$set": {"z": 1}}, "multi": True, "sort": {"v": 1}}],
            }
        )
    details = ei.value.details or {}
    assert details.get("code") == 9
    assert details["errmsg"] == "Cannot specify sort with multi=true"


def test_update_sort_still_upserts_when_nothing_matches(client: MongoClient) -> None:
    client.drop_database("srt")
    coll = client.srt.c
    coll.insert_one({"_id": 1, "v": 1})
    res = coll.update_one({"v": 99}, {"$set": {"z": 1}}, upsert=True, sort={"v": 1})
    assert res.upserted_id is not None


# --- argument validation, probed against mongod 8.2.11 on 2026-08-31 --------
#
# A differential sweep of 47 shapes found five divergences, all of them error
# SHAPE: a missing `nsInfo` reported as a wrong type, a negative namespace
# index and an unknown op kind reported with our own wording, a non-array
# `nsInfo` reported as a batch-size problem, and an invalid namespace reported
# as a bad index. Each expectation below is mongod's, verbatim.


def _bw_error(client: MongoClient, cmd: dict):
    from pymongo.errors import OperationFailure

    with pytest.raises(OperationFailure) as exc:
        client["admin"].command(cmd)
    return exc.value


def _ops(op: dict) -> dict:
    return {"bulkWrite": 1, "ops": [op], "nsInfo": [{"ns": "bwdb.c"}]}


def test_missing_nsinfo_is_a_missing_field_not_a_type_error(client: MongoClient) -> None:
    err = _bw_error(client, {"bulkWrite": 1, "ops": [{"insert": 0, "document": {"_id": 1}}]})
    assert err.code == 40414
    assert err.details["errmsg"] == "BSON field 'bulkWrite.nsInfo' is missing but a required field"


def test_null_nsinfo_reads_as_missing(client: MongoClient) -> None:
    err = _bw_error(
        client, {"bulkWrite": 1, "ops": [{"insert": 0, "document": {"_id": 1}}], "nsInfo": None}
    )
    assert err.code == 40414


def test_nsinfo_wrong_type_is_reported_before_the_batch_size(client: MongoClient) -> None:
    """`{ops: [], nsInfo: 5}` is an nsInfo type error, NOT "Got 0 operations".

    Our order had the batch-size check first, so a wrong-typed nsInfo reported
    a batch-size problem. mongod validates nsInfo first.
    """
    err = _bw_error(client, {"bulkWrite": 1, "ops": [], "nsInfo": 5})
    assert err.code == 14
    assert (
        err.details["errmsg"]
        == "BSON field 'bulkWrite.nsInfo' is the wrong type 'int', expected type 'array'"
    )


def test_nsinfo_entry_wrong_type_names_its_index(client: MongoClient) -> None:
    err = _bw_error(
        client, {"bulkWrite": 1, "ops": [{"insert": 0, "document": {"_id": 1}}], "nsInfo": [5]}
    )
    assert err.code == 14
    assert (
        err.details["errmsg"]
        == "BSON field 'bulkWrite.nsInfo.0' is the wrong type 'int', expected type 'object'"
    )


def test_nsinfo_unknown_field_does_not_carry_an_index(client: MongoClient) -> None:
    """The ENTRY error is indexed (`nsInfo.0`); the FIELD error is not
    (`nsInfo.x`). mongod's own inconsistency, reproduced rather than tidied."""
    err = _bw_error(
        client,
        {"bulkWrite": 1, "ops": [{"insert": 0, "document": {"_id": 1}}], "nsInfo": [{"x": 1}]},
    )
    assert err.code == 40415
    assert err.details["errmsg"] == "BSON field 'bulkWrite.nsInfo.x' is an unknown field."


@pytest.mark.parametrize(("ns", "named"), [("nodot", "nodot"), ("", ""), (".", "")])
def test_invalid_namespace_is_its_own_error(client: MongoClient, ns: str, named: str) -> None:
    """mongod names the DATABASE half: 'nodot' has no dot so the whole string
    is the db, while '.' and '' both report ''."""
    err = _bw_error(
        client,
        {"bulkWrite": 1, "ops": [{"insert": 0, "document": {"_id": 1}}], "nsInfo": [{"ns": ns}]},
    )
    assert err.code == 73
    assert err.details["errmsg"] == f"Invalid namespace specified for bulkWrite: '{named}'"


def test_negative_namespace_index_names_the_op_kind(client: MongoClient) -> None:
    """The field name is the bare op kind (`insert`), not the IDL path."""
    err = _bw_error(client, _ops({"insert": -1, "document": {"_id": 1}}))
    assert err.code == 2
    assert err.details["errmsg"] == "BSON field 'insert' value must be >= 0, actual value '-1'"


def test_out_of_range_namespace_index_is_the_index_error(client: MongoClient) -> None:
    err = _bw_error(client, _ops({"insert": 5, "document": {"_id": 1}}))
    assert err.code == 2
    assert "has an invalid nsInfo index." in err.details["errmsg"]


def test_wrong_typed_namespace_index(client: MongoClient) -> None:
    err = _bw_error(client, _ops({"insert": "0", "document": {"_id": 1}}))
    assert err.code == 14
    assert err.details["errmsg"] == (
        "BSON field 'bulkWrite.ops.insert' is the wrong type 'string', "
        "expected types '[long, int, decimal, double]'"
    )


def test_a_double_namespace_index_is_accepted(client: MongoClient) -> None:
    """0.0 is a valid index -- the type list admits doubles."""
    client["admin"].command(_ops({"insert": 0.0, "document": {"_id": 1}}))
    assert client["bwdb"].c.find_one({"_id": 1}) is not None


def test_unknown_op_kind_names_the_key(client: MongoClient) -> None:
    err = _bw_error(client, _ops({"frobnicate": 0, "document": {"_id": 1}}))
    assert err.code == 40415
    assert err.details["errmsg"] == "BSON field 'bulkWrite.frobnicate' is an unknown field."


@pytest.mark.parametrize(
    ("op", "missing"),
    [
        ({"insert": 0}, "document"),
        ({"update": 0, "filter": {}}, "updateMods"),
        # `filter` is required on both: we defaulted it to {}, silently turning
        # a malformed op into a match-all.
        ({"update": 0, "updateMods": {"$set": {"a": 1}}}, "filter"),
        ({"delete": 0}, "filter"),
    ],
)
def test_missing_op_fields(client: MongoClient, op: dict, missing: str) -> None:
    err = _bw_error(client, _ops(op))
    assert err.code == 40414
    assert err.details["errmsg"] == (
        f"BSON field 'bulkWrite.ops.{missing}' is missing but a required field"
    )


def test_insert_document_must_be_an_object(client: MongoClient) -> None:
    err = _bw_error(client, _ops({"insert": 0, "document": 5}))
    assert err.code == 14
    assert (
        err.details["errmsg"]
        == "BSON field 'bulkWrite.ops.document' is the wrong type 'int', expected type 'object'"
    )
