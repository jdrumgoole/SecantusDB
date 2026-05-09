"""Unit tests for ``secantus.rbac`` — built-in role privileges and the
``check_privilege`` decision function.

End-to-end RBAC behaviour through the wire is in ``tests/test_auth.py``;
this file exercises the pure module so the privilege table is testable
without spinning up a server.
"""

from __future__ import annotations

import pytest

from secantus.rbac import (
    A_CREATE_COLLECTION,
    A_CREATE_INDEX,
    A_CREATE_ROLE,
    A_CREATE_USER,
    A_DROP_DATABASE,
    A_FIND,
    A_FSYNC,
    A_GET_LOG,
    A_INSERT,
    A_LIST_COLLECTIONS,
    A_LIST_DATABASES,
    A_REMOVE,
    A_SERVER_STATUS,
    A_UPDATE,
    A_VIEW_ROLE,
    BUILT_IN_ROLES,
    check_privilege,
    is_known_role,
    role_grants_action,
)


class TestRoleSpecs:
    def test_builtin_role_names(self) -> None:
        # The MVP built-in set; future additions are welcome but these
        # eight are load-bearing for typical pymongo workloads.
        assert {
            "read",
            "readWrite",
            "dbAdmin",
            "userAdmin",
            "readAnyDatabase",
            "readWriteAnyDatabase",
            "dbAdminAnyDatabase",
            "userAdminAnyDatabase",
            "root",
        } <= set(BUILT_IN_ROLES)

    def test_is_known_role(self) -> None:
        assert is_known_role("read")
        assert is_known_role("root")
        assert not is_known_role("admin")  # ← that's a *db*, not a role
        assert not is_known_role("notARole")


class TestRoleGrantsAction:
    def test_read_on_bound_db(self) -> None:
        assert role_grants_action("read", "myapp", A_FIND, target_db="myapp")
        # Different db: no grant.
        assert not role_grants_action("read", "myapp", A_FIND, target_db="other")

    def test_read_does_not_grant_writes(self) -> None:
        assert not role_grants_action("read", "myapp", A_INSERT, target_db="myapp")
        assert not role_grants_action("read", "myapp", A_UPDATE, target_db="myapp")
        assert not role_grants_action("read", "myapp", A_REMOVE, target_db="myapp")

    def test_readWrite_grants_writes(self) -> None:
        assert role_grants_action("readWrite", "myapp", A_INSERT, target_db="myapp")
        assert role_grants_action("readWrite", "myapp", A_UPDATE, target_db="myapp")

    def test_readAnyDatabase_must_be_admin_bound(self) -> None:
        # mongod requires *AnyDatabase roles to be granted via admin.
        # A binding to a non-admin db is invalid; nothing's granted.
        assert role_grants_action("readAnyDatabase", "admin", A_FIND, target_db="anything")
        assert not role_grants_action("readAnyDatabase", "myapp", A_FIND, target_db="myapp")

    def test_readAnyDatabase_spans_all_dbs(self) -> None:
        assert role_grants_action("readAnyDatabase", "admin", A_FIND, target_db="dbA")
        assert role_grants_action("readAnyDatabase", "admin", A_FIND, target_db="dbB")

    def test_root_grants_everything(self) -> None:
        # CRUD across any db, plus cluster-wide actions.
        for db in ("anyDb", "another"):
            assert role_grants_action("root", "admin", A_FIND, target_db=db)
            assert role_grants_action("root", "admin", A_INSERT, target_db=db)
            assert role_grants_action("root", "admin", A_DROP_DATABASE, target_db=db)
            assert role_grants_action("root", "admin", A_CREATE_USER, target_db=db)
        assert role_grants_action("root", "admin", A_SERVER_STATUS, target_db=None, cluster=True)
        assert role_grants_action("root", "admin", A_GET_LOG, target_db=None, cluster=True)

    def test_userAdmin_does_not_grant_data_actions(self) -> None:
        assert role_grants_action("userAdmin", "admin", A_CREATE_USER, target_db="admin")
        # …but not read/write on collections.
        assert not role_grants_action("userAdmin", "admin", A_FIND, target_db="admin")
        assert not role_grants_action("userAdmin", "admin", A_INSERT, target_db="admin")

    def test_unknown_role_grants_nothing(self) -> None:
        assert not role_grants_action("noSuchRole", "admin", A_FIND, target_db="admin")


class TestCheckPrivilege:
    def test_no_roles_no_privileges(self) -> None:
        assert not check_privilege([], A_FIND, target_db="myapp")

    def test_first_grant_wins(self) -> None:
        roles = [{"role": "read", "db": "myapp"}, {"role": "userAdmin", "db": "admin"}]
        assert check_privilege(roles, A_FIND, target_db="myapp")
        assert check_privilege(roles, A_CREATE_USER, target_db="admin")

    def test_grants_combine_for_different_dbs(self) -> None:
        # A user with read on dbA and readWrite on dbB has different
        # access in each.
        roles = [
            {"role": "read", "db": "dbA"},
            {"role": "readWrite", "db": "dbB"},
        ]
        assert check_privilege(roles, A_FIND, target_db="dbA")
        assert not check_privilege(roles, A_INSERT, target_db="dbA")
        assert check_privilege(roles, A_FIND, target_db="dbB")
        assert check_privilege(roles, A_INSERT, target_db="dbB")

    def test_root_cluster_action(self) -> None:
        assert check_privilege([{"role": "root", "db": "admin"}], A_SERVER_STATUS, cluster=True)

    def test_non_root_cluster_action_denied(self) -> None:
        # readWrite has no cluster grant.
        assert not check_privilege(
            [{"role": "readWrite", "db": "myapp"}], A_SERVER_STATUS, cluster=True
        )

    def test_malformed_role_entries_ignored(self) -> None:
        # A garbage entry shouldn't crash the check; just treat as empty.
        assert not check_privilege(
            ["not a dict", {"role": 123}, {"db": "x"}],
            A_FIND,
            target_db="x",
        )


class TestPrivilegeMatrix:
    """Spot checks against the documented matrix in
    ``src/secantus/rbac.py``.

    Each row asserts the canonical action for each role does what the
    docstring claims; protects against drift if the action sets are
    edited without consulting the table."""

    @pytest.mark.parametrize(
        "role,action,allowed",
        [
            ("read", A_FIND, True),
            ("read", A_INSERT, False),
            ("readWrite", A_FIND, True),
            ("readWrite", A_INSERT, True),
            ("readWrite", A_UPDATE, True),
            ("readWrite", A_REMOVE, True),
            ("dbAdmin", A_FIND, False),  # dbAdmin can list/manage but not read data
            ("dbAdmin", A_VIEW_ROLE, True),
            ("userAdmin", A_FIND, False),
            ("userAdmin", A_CREATE_USER, True),
            ("root", A_FIND, True),
            ("root", A_INSERT, True),
            ("root", A_CREATE_USER, True),
            ("root", A_DROP_DATABASE, True),
        ],
    )
    def test_matrix_row(self, role: str, action: str, allowed: bool) -> None:
        bound_db = "admin" if BUILT_IN_ROLES[role].admin_only else "myapp"
        actual = role_grants_action(role, bound_db, action, target_db=bound_db)
        assert actual is allowed, (
            f"{role} on {bound_db} → {action}: expected {allowed}, got {actual}"
        )


class TestClusterRoleBundles:
    """``clusterMonitor`` / ``clusterAdmin`` / ``backup`` / ``restore`` —
    mongod's named cluster-wide bundles. All admin-bound, all span every
    db. Spot checks each role's canonical actions land where they should
    and cluster-write actions stay gated to ``clusterAdmin``/``restore``."""

    def test_clusterMonitor_grants_listDatabases(self) -> None:
        assert role_grants_action(
            "clusterMonitor", "admin", A_LIST_DATABASES, target_db=None, cluster=True
        )
        assert role_grants_action(
            "clusterMonitor", "admin", A_SERVER_STATUS, target_db=None, cluster=True
        )

    def test_clusterMonitor_does_not_grant_dropDatabase(self) -> None:
        # Read-only monitoring; dropDatabase is a write that requires
        # clusterAdmin / root / restore.
        assert not role_grants_action("clusterMonitor", "admin", A_DROP_DATABASE, target_db="anyDb")

    def test_clusterMonitor_must_be_admin_bound(self) -> None:
        # Cluster bundles, like the *AnyDatabase variants, are admin-only.
        assert not role_grants_action(
            "clusterMonitor", "myapp", A_LIST_DATABASES, target_db=None, cluster=True
        )

    def test_clusterAdmin_grants_dropDatabase(self) -> None:
        assert role_grants_action("clusterAdmin", "admin", A_DROP_DATABASE, target_db="anyDb")
        assert role_grants_action("clusterAdmin", "admin", A_FSYNC, target_db=None, cluster=True)
        # And inherits clusterMonitor's reads.
        assert role_grants_action(
            "clusterAdmin", "admin", A_SERVER_STATUS, target_db=None, cluster=True
        )

    def test_clusterAdmin_does_not_grant_collection_writes(self) -> None:
        # Cluster admin manages cluster topology, not collection data.
        assert not role_grants_action("clusterAdmin", "admin", A_INSERT, target_db="anyDb")
        assert not role_grants_action("clusterAdmin", "admin", A_FIND, target_db="anyDb")

    def test_backup_reads_every_database(self) -> None:
        for db in ("dbA", "dbB", "admin"):
            assert role_grants_action("backup", "admin", A_FIND, target_db=db)
            assert role_grants_action("backup", "admin", A_LIST_COLLECTIONS, target_db=db)
        # Backup needs to see what's there to dump it.
        assert role_grants_action("backup", "admin", A_LIST_DATABASES, target_db=None, cluster=True)

    def test_backup_does_not_grant_writes(self) -> None:
        # A backup user shouldn't be able to mutate data or rotate creds.
        assert not role_grants_action("backup", "admin", A_INSERT, target_db="dbA")
        assert not role_grants_action("backup", "admin", A_DROP_DATABASE, target_db="dbA")
        assert not role_grants_action("backup", "admin", A_CREATE_USER, target_db="admin")

    def test_restore_writes_every_database(self) -> None:
        for db in ("dbA", "dbB", "admin"):
            assert role_grants_action("restore", "admin", A_INSERT, target_db=db)
            assert role_grants_action("restore", "admin", A_CREATE_COLLECTION, target_db=db)
            assert role_grants_action("restore", "admin", A_CREATE_INDEX, target_db=db)
        # Restore recreates users/roles as part of bringing dumps back online.
        assert role_grants_action("restore", "admin", A_CREATE_USER, target_db="admin")
        assert role_grants_action("restore", "admin", A_CREATE_ROLE, target_db="admin")
        # And dropDatabase to make room for the import.
        assert role_grants_action("restore", "admin", A_DROP_DATABASE, target_db="dbA")

    def test_cluster_bundles_in_known_roles(self) -> None:
        for name in ("clusterMonitor", "clusterAdmin", "backup", "restore"):
            assert is_known_role(name), name
            assert name in BUILT_IN_ROLES
