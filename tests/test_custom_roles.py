"""Custom roles: createRole / dropRole / grantPrivilegesToRole /
revokePrivilegesFromRole / grantRolesToRole / revokeRolesFromRole /
rolesInfo (custom-role surface) / dropAllRolesFromDatabase, plus the
RBAC integration that walks custom roles + their inherited graph
when checking privileges.

Three layers:

1. Pure unit tests on ``secantus.rbac.check_privilege`` driving a
   role-resolver callable directly (no server, no pymongo). Verify
   the inheritance walk + cycle detection without WiredTiger.

2. Storage round-trip (``add_role`` / ``get_role`` / ``drop_role`` /
   ``list_roles``). Pure ``Storage`` calls.

3. End-to-end via pymongo against ``SecantusDBServer`` with auth
   enabled — provisioning a role, granting it to a user, observing
   the privilege fire on actual commands.
"""

from __future__ import annotations

import pymongo
import pytest
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer
from secantus.auth import SCRAM_SHA_256, derive_credentials
from secantus.rbac import check_privilege
from secantus.storage import Storage

# -----------------------------------------------------------------
# Unit: rbac.check_privilege with custom-role resolver
# -----------------------------------------------------------------


def _resolver_for(records: list[dict]) -> callable:
    """Build a ``(db, name) -> record`` lookup matching
    ``Storage.get_role``'s signature."""
    by_key = {(r["db"], r["role"]): r for r in records}
    return lambda db, name: by_key.get((db, name))


def test_check_privilege_resolves_simple_custom_role() -> None:
    """A custom role with one privilege grants the matching action."""
    records = [
        {
            "role": "auditor",
            "db": "shop",
            "privileges": [{"resource": {"db": "shop", "collection": ""}, "actions": ["find"]}],
            "roles": [],
        }
    ]
    assert check_privilege(
        [{"role": "auditor", "db": "shop"}],
        "find",
        target_db="shop",
        role_resolver=_resolver_for(records),
    )
    # Different db: no privilege.
    assert not check_privilege(
        [{"role": "auditor", "db": "shop"}],
        "find",
        target_db="other",
        role_resolver=_resolver_for(records),
    )
    # Different action: no privilege.
    assert not check_privilege(
        [{"role": "auditor", "db": "shop"}],
        "insert",
        target_db="shop",
        role_resolver=_resolver_for(records),
    )


def test_check_privilege_walks_inherited_custom_role() -> None:
    """Inherited custom roles add their privileges to the parent."""
    records = [
        {
            "role": "auditor",
            "db": "shop",
            "privileges": [{"resource": {"db": "shop", "collection": ""}, "actions": ["find"]}],
            "roles": [],
        },
        {
            "role": "manager",
            "db": "shop",
            "privileges": [{"resource": {"db": "shop", "collection": ""}, "actions": ["insert"]}],
            "roles": [{"role": "auditor", "db": "shop"}],
        },
    ]
    # Manager has its own insert + inherited find.
    assert check_privilege(
        [{"role": "manager", "db": "shop"}],
        "find",
        target_db="shop",
        role_resolver=_resolver_for(records),
    )
    assert check_privilege(
        [{"role": "manager", "db": "shop"}],
        "insert",
        target_db="shop",
        role_resolver=_resolver_for(records),
    )


def test_check_privilege_walks_inherited_built_in_role() -> None:
    """A custom role can inherit a built-in (`read`) and pick up its actions."""
    records = [
        {
            "role": "shopReader",
            "db": "shop",
            "privileges": [],
            "roles": [{"role": "read", "db": "shop"}],
        }
    ]
    assert check_privilege(
        [{"role": "shopReader", "db": "shop"}],
        "find",
        target_db="shop",
        role_resolver=_resolver_for(records),
    )


def test_check_privilege_cycle_detection() -> None:
    """A → B → A cycle terminates without infinite loop. The privilege
    closure is still computed correctly (both sides' actions union)."""
    records = [
        {
            "role": "A",
            "db": "shop",
            "privileges": [{"resource": {"db": "shop", "collection": ""}, "actions": ["find"]}],
            "roles": [{"role": "B", "db": "shop"}],
        },
        {
            "role": "B",
            "db": "shop",
            "privileges": [{"resource": {"db": "shop", "collection": ""}, "actions": ["insert"]}],
            "roles": [{"role": "A", "db": "shop"}],
        },
    ]
    assert check_privilege(
        [{"role": "A", "db": "shop"}],
        "find",
        target_db="shop",
        role_resolver=_resolver_for(records),
    )
    assert check_privilege(
        [{"role": "A", "db": "shop"}],
        "insert",
        target_db="shop",
        role_resolver=_resolver_for(records),
    )


def test_check_privilege_anyresource() -> None:
    """``{anyResource: true}`` matches any target_db."""
    records = [
        {
            "role": "superduper",
            "db": "admin",
            "privileges": [{"resource": {"anyResource": True}, "actions": ["serverStatus"]}],
            "roles": [],
        }
    ]
    # Cluster action.
    assert check_privilege(
        [{"role": "superduper", "db": "admin"}],
        "serverStatus",
        cluster=True,
        role_resolver=_resolver_for(records),
    )
    # DB-scoped action with random db.
    assert check_privilege(
        [{"role": "superduper", "db": "admin"}],
        "serverStatus",
        target_db="random",
        role_resolver=_resolver_for(records),
    )


def test_check_privilege_cluster_resource() -> None:
    """``{cluster: true}`` only matches when ``cluster=True`` in the call."""
    records = [
        {
            "role": "clusterMon",
            "db": "admin",
            "privileges": [{"resource": {"cluster": True}, "actions": ["serverStatus"]}],
            "roles": [],
        }
    ]
    assert check_privilege(
        [{"role": "clusterMon", "db": "admin"}],
        "serverStatus",
        cluster=True,
        role_resolver=_resolver_for(records),
    )
    # DB-scoped check should NOT see the cluster privilege.
    assert not check_privilege(
        [{"role": "clusterMon", "db": "admin"}],
        "serverStatus",
        target_db="x",
        role_resolver=_resolver_for(records),
    )


def test_check_privilege_unknown_custom_role_grants_nothing() -> None:
    """Unknown custom role with no resolver hit grants nothing."""
    assert not check_privilege(
        [{"role": "ghost", "db": "x"}],
        "find",
        target_db="x",
        role_resolver=lambda *_: None,
    )


# -----------------------------------------------------------------
# Storage round-trip
# -----------------------------------------------------------------


def test_storage_role_round_trip(tmp_path) -> None:
    storage = Storage(str(tmp_path / "wt"), ttl_sweep_seconds=0)
    try:
        record = {
            "_id": "shop.auditor",
            "role": "auditor",
            "db": "shop",
            "privileges": [{"resource": {"db": "shop", "collection": ""}, "actions": ["find"]}],
            "roles": [],
        }
        assert storage.add_role("shop", "auditor", record)
        # Duplicate without replace fails.
        assert not storage.add_role("shop", "auditor", record)
        # Read back.
        got = storage.get_role("shop", "auditor")
        assert got is not None
        assert got["role"] == "auditor"
        assert got["privileges"][0]["actions"] == ["find"]
        # Drop.
        assert storage.drop_role("shop", "auditor")
        assert storage.get_role("shop", "auditor") is None
        # Drop missing returns False.
        assert not storage.drop_role("shop", "auditor")
    finally:
        storage.close()


def test_storage_list_roles_filters_by_db(tmp_path) -> None:
    storage = Storage(str(tmp_path / "wt"), ttl_sweep_seconds=0)
    try:
        for db in ("a", "b", "a"):
            storage.add_role(
                db,
                f"role_{db}_{db}",
                {"_id": f"{db}.role_{db}", "role": f"role_{db}", "db": db},
            )
        a_only = storage.list_roles("a")
        assert {r["role"] for r in a_only} == {"role_a"}
        b_only = storage.list_roles("b")
        assert {r["role"] for r in b_only} == {"role_b"}
        all_dbs = storage.list_roles()
        assert len(all_dbs) == 2
    finally:
        storage.close()


# -----------------------------------------------------------------
# End-to-end via pymongo
# -----------------------------------------------------------------


@pytest.fixture
def server_with_auth(tmp_path):
    """Auth-enabled server with a bootstrap root user on admin."""
    srv = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "wt"),
        require_auth=True,
        ttl_sweep_seconds=0,
    )
    srv.start()
    creds = derive_credentials("secret")
    srv.storage.add_user(
        "admin",
        "root",
        {
            "_id": "admin.root",
            "user": "root",
            "db": "admin",
            "credentials": creds.to_doc(),
            "roles": [{"role": "root", "db": "admin"}],
            "mechanisms": [SCRAM_SHA_256],
        },
    )
    try:
        yield srv
    finally:
        srv.stop()


def _client(srv, user="root", pwd="secret", db="admin"):
    return pymongo.MongoClient(
        srv.uri,
        username=user,
        password=pwd,
        authSource=db,
        authMechanism=SCRAM_SHA_256,
        serverSelectionTimeoutMS=5000,
    )


def test_create_role_then_grant_to_user_then_use(server_with_auth) -> None:
    """End-to-end: create a custom role with `find` on db `shop`,
    create a user with that role, verify the user can `find` on shop
    but cannot `insert`.
    """
    root = _client(server_with_auth)
    try:
        root["shop"].command(
            "createRole",
            "shopAuditor",
            privileges=[{"resource": {"db": "shop", "collection": ""}, "actions": ["find"]}],
            roles=[],
        )
        root["shop"].command(
            "createUser",
            "auditor",
            pwd="p",
            roles=[{"role": "shopAuditor", "db": "shop"}],
        )
    finally:
        root.close()

    cli = _client(server_with_auth, user="auditor", pwd="p", db="shop")
    try:
        # find: allowed.
        list(cli["shop"]["items"].find({}))
        # insert: not in the role's privileges.
        with pytest.raises(OperationFailure) as exc:
            cli["shop"]["items"].insert_one({"x": 1})
        assert exc.value.code == 13
    finally:
        cli.close()


def test_grant_privileges_to_role_extends_actions(server_with_auth) -> None:
    """grantPrivilegesToRole adds an action to an existing role."""
    root = _client(server_with_auth)
    try:
        root["shop"].command(
            "createRole",
            "shopAuditor2",
            privileges=[{"resource": {"db": "shop", "collection": ""}, "actions": ["find"]}],
            roles=[],
        )
        root["shop"].command(
            "createUser",
            "auditor2",
            pwd="p",
            roles=[{"role": "shopAuditor2", "db": "shop"}],
        )

        # Grant insert on top of find.
        root["shop"].command(
            "grantPrivilegesToRole",
            "shopAuditor2",
            privileges=[{"resource": {"db": "shop", "collection": ""}, "actions": ["insert"]}],
        )
    finally:
        root.close()

    # Open a fresh connection to pick up the new privilege.
    cli = _client(server_with_auth, user="auditor2", pwd="p", db="shop")
    try:
        cli["shop"]["items"].insert_one({"x": 1})  # should succeed now
        assert cli["shop"]["items"].find_one({"x": 1}) is not None
    finally:
        cli.close()


def test_drop_role_revokes_privileges(server_with_auth) -> None:
    """Dropping the role behind a user's grant removes the privileges."""
    root = _client(server_with_auth)
    try:
        root["shop"].command(
            "createRole",
            "transient",
            privileges=[{"resource": {"db": "shop", "collection": ""}, "actions": ["find"]}],
            roles=[],
        )
        root["shop"].command(
            "createUser",
            "tempu",
            pwd="p",
            roles=[{"role": "transient", "db": "shop"}],
        )
    finally:
        root.close()

    cli = _client(server_with_auth, user="tempu", pwd="p", db="shop")
    try:
        # Works.
        list(cli["shop"]["items"].find({}))
    finally:
        cli.close()

    # Drop the role.
    root = _client(server_with_auth)
    try:
        root["shop"].command("dropRole", "transient")
    finally:
        root.close()

    # Reconnect — the role binding still exists on the user record but
    # resolves to None now, so the find should fail.
    cli = _client(server_with_auth, user="tempu", pwd="p", db="shop")
    try:
        with pytest.raises(OperationFailure) as exc:
            list(cli["shop"]["items"].find({}))
        assert exc.value.code == 13
    finally:
        cli.close()


def test_create_role_rejects_built_in_name(server_with_auth) -> None:
    """``createRole: "read"`` is rejected — built-in name collision."""
    root = _client(server_with_auth)
    try:
        with pytest.raises(OperationFailure) as exc:
            root["shop"].command(
                "createRole",
                "read",
                privileges=[],
                roles=[],
            )
        assert exc.value.code == 2  # BadValue
    finally:
        root.close()


def test_drop_all_roles_from_database(server_with_auth) -> None:
    """``dropAllRolesFromDatabase`` removes every custom role on the db
    and returns ``n``. Built-ins aren't affected."""
    root = _client(server_with_auth)
    try:
        for r in ("alpha", "beta", "gamma"):
            root["shop"].command("createRole", r, privileges=[], roles=[])
        result = root["shop"].command("dropAllRolesFromDatabase")
        assert result["n"] == 3
        info = root["shop"].command("rolesInfo", 1)
        assert info["roles"] == []
    finally:
        root.close()


def test_inherited_role_grants_actions(server_with_auth) -> None:
    """Custom role inheriting from `read` (built-in) gets `find`."""
    root = _client(server_with_auth)
    try:
        root["shop"].command(
            "createRole",
            "readPlus",
            privileges=[],
            roles=[{"role": "read", "db": "shop"}],
        )
        root["shop"].command(
            "createUser",
            "rp",
            pwd="p",
            roles=[{"role": "readPlus", "db": "shop"}],
        )
    finally:
        root.close()

    cli = _client(server_with_auth, user="rp", pwd="p", db="shop")
    try:
        list(cli["shop"]["items"].find({}))  # works through inheritance
    finally:
        cli.close()
