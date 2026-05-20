"""``admin.system.users`` is a synthetic read-only view onto the
user store.

Before this slice, credentials lived in a dedicated WT table
(``secantus_users``) but ``find`` / ``aggregate`` / ``count`` on
``admin.system.users`` returned nothing — they searched the empty
regular doc table. Tools and driver tests that introspect users
via ``admin.system.users.find()`` saw no rows.

This slice surfaces the records under that namespace with the same
mongod-shaped fields the records already carry (``_id`` =
``"<db>.<user>"``, ``user``, ``db``, ``credentials``, ``roles``,
``mechanisms``). Writes to the synthetic namespace are rejected
with mongod's ``Unauthorized`` (code 13) — the view is read-only;
mutate through ``createUser`` / ``updateUser`` / ``dropUser``.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


def _make_user(client, db: str, name: str, pwd: str = "pw", roles=None) -> None:
    client[db].command(
        "createUser",
        name,
        pwd=pwd,
        roles=roles or [],
    )


# ---------------------------------------------------------------------------
# Read path: find / aggregate / count
# ---------------------------------------------------------------------------


def test_find_admin_system_users_returns_created_users(client) -> None:
    """createUser → admin.system.users.find() returns the record."""
    _make_user(client, "admin", "alice")
    _make_user(client, "admin", "bob")

    docs = list(client["admin"]["system.users"].find())
    names = sorted(d["user"] for d in docs)
    assert names == ["alice", "bob"]

    # mongod-shaped fields are all present.
    for d in docs:
        assert d["_id"] == f"admin.{d['user']}"
        assert d["db"] == "admin"
        assert d["roles"] == []
        assert "SCRAM-SHA-256" in d.get("mechanisms", [])
        assert "credentials" in d


def test_find_users_across_databases(client) -> None:
    """Users created against different databases all surface under
    admin.system.users (mongod's storage layout — users live in
    admin.system.users regardless of their auth db)."""
    _make_user(client, "admin", "root")
    _make_user(client, "appdb", "appuser")
    _make_user(client, "otherdb", "otheruser")

    docs = list(client["admin"]["system.users"].find())
    rows = sorted((d["db"], d["user"]) for d in docs)
    assert rows == [("admin", "root"), ("appdb", "appuser"), ("otherdb", "otheruser")]


def test_find_with_filter_on_db_field(client) -> None:
    """Filter by the per-user `db` field — drivers use this to scope
    user listings to a single auth database."""
    _make_user(client, "admin", "root")
    _make_user(client, "appdb", "appuser1")
    _make_user(client, "appdb", "appuser2")

    docs = list(client["admin"]["system.users"].find({"db": "appdb"}))
    assert sorted(d["user"] for d in docs) == ["appuser1", "appuser2"]


def test_find_with_projection(client) -> None:
    """Projection drops credentials (the sensitive field) just like
    mongod when ``showCredentials`` isn't set — but this is a generic
    `find` projection, not the usersInfo flag. The user is in control."""
    _make_user(client, "admin", "alice")

    docs = list(
        client["admin"]["system.users"].find({"user": "alice"}, projection={"credentials": 0})
    )
    assert len(docs) == 1
    assert "credentials" not in docs[0]
    assert docs[0]["user"] == "alice"


def test_count_documents(client) -> None:
    """`countDocuments` on the view delegates through the count helper."""
    _make_user(client, "admin", "a")
    _make_user(client, "admin", "b")
    _make_user(client, "admin", "c")

    n = client["admin"]["system.users"].count_documents({})
    assert n == 3

    n_filtered = client["admin"]["system.users"].count_documents({"user": "b"})
    assert n_filtered == 1


def test_aggregate_pipeline(client) -> None:
    """`$match` + `$group` work — the read path lifts the leading
    `$match` and the rest runs through the regular pipeline engine."""
    _make_user(client, "admin", "root")
    _make_user(client, "appdb", "u1")
    _make_user(client, "appdb", "u2")

    pipeline = [
        {"$match": {"db": "appdb"}},
        {"$group": {"_id": "$db", "n": {"$sum": 1}}},
    ]
    docs = list(client["admin"]["system.users"].aggregate(pipeline))
    assert docs == [{"_id": "appdb", "n": 2}]


def test_other_db_system_users_is_empty(client) -> None:
    """Only `admin.system.users` is the synthetic view — querying
    `mydb.system.users` directly returns nothing (matches mongod's
    storage layout)."""
    _make_user(client, "admin", "alice")

    docs = list(client["mydb"]["system.users"].find())
    assert docs == []


# ---------------------------------------------------------------------------
# Write path: rejected with code 13
# ---------------------------------------------------------------------------


def test_insert_into_system_users_rejected(client) -> None:
    """Direct insert into admin.system.users is rejected (code 13).
    Real mongod has the same rejection via RBAC; we mirror so debuggers
    don't end up with ghost rows in the doc table that the read view
    can't see."""
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"]["system.users"].insert_one(
            {"_id": "admin.eve", "user": "eve", "db": "admin"}
        )
    assert exc_info.value.code == 13


def test_update_on_system_users_rejected(client) -> None:
    _make_user(client, "admin", "alice")
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"]["system.users"].update_one({"user": "alice"}, {"$set": {"hacked": True}})
    assert exc_info.value.code == 13


def test_delete_on_system_users_rejected(client) -> None:
    _make_user(client, "admin", "alice")
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"]["system.users"].delete_one({"user": "alice"})
    assert exc_info.value.code == 13


def test_drop_collection_rejected(client) -> None:
    _make_user(client, "admin", "alice")
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"].drop_collection("system.users")
    assert exc_info.value.code == 13


# ---------------------------------------------------------------------------
# Interaction with the canonical commands
# ---------------------------------------------------------------------------


def test_dropuser_removes_from_view(client) -> None:
    """`dropUser` against the regular command path removes the record
    from the view too — same underlying store."""
    _make_user(client, "admin", "alice")
    assert client["admin"]["system.users"].count_documents({}) == 1

    client["admin"].command("dropUser", "alice")
    assert client["admin"]["system.users"].count_documents({}) == 0


def test_updateuser_role_change_reflected_in_view(client) -> None:
    """`updateUser` modifying roles surfaces in the view."""
    _make_user(client, "admin", "alice", roles=[])
    client["admin"].command("updateUser", "alice", roles=["read"])

    docs = list(client["admin"]["system.users"].find({"user": "alice"}))
    assert len(docs) == 1
    assert docs[0]["roles"] == [{"role": "read", "db": "admin"}]
