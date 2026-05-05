"""Unit tests for ``secantus.rbac`` — built-in role privileges and the
``check_privilege`` decision function.

End-to-end RBAC behaviour through the wire is in ``tests/test_auth.py``;
this file exercises the pure module so the privilege table is testable
without spinning up a server.
"""

from __future__ import annotations

import pytest

from secantus.rbac import (
    A_CREATE_USER,
    A_DROP_DATABASE,
    A_FIND,
    A_GET_LOG,
    A_INSERT,
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
