"""Role membership (#138): GRANT <role> TO <member> / REVOKE, recorded per
(role, member) and reflected through pg_catalog.pg_auth_members (joining to
pg_roles by oid). Driven over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    sess = Session(database=DB, user="postgres")

    def q(sql):
        run_sql(s, DB, sql, session=sess)

    for role in ("readers", "writers", "alice", "bob"):
        q(f"CREATE ROLE {role}")
    try:
        yield s
    finally:
        s.close()


def _run(storage, sql, user="postgres"):
    return run_sql(storage, DB, sql, session=Session(database=DB, user=user))[-1]


_MEMBERS_SQL = (
    "SELECT r.rolname, m.rolname, am.admin_option "
    "FROM pg_catalog.pg_auth_members am "
    "JOIN pg_catalog.pg_roles r ON r.oid = am.roleid "
    "JOIN pg_catalog.pg_roles m ON m.oid = am.member "
    "ORDER BY r.rolname, m.rolname"
)


def test_grant_and_reflect(storage):
    assert _run(storage, "GRANT readers TO alice").command_tag == "GRANT ROLE"
    assert _run(storage, _MEMBERS_SQL).rows == [("readers", "alice", False)]


def test_grant_with_admin_option(storage):
    _run(storage, "GRANT writers TO bob WITH ADMIN OPTION")
    assert _run(storage, _MEMBERS_SQL).rows == [("writers", "bob", True)]


def test_grant_multiple_roles_and_members(storage):
    _run(storage, "GRANT readers, writers TO alice, bob")
    assert _run(storage, _MEMBERS_SQL).rows == [
        ("readers", "alice", False),
        ("readers", "bob", False),
        ("writers", "alice", False),
        ("writers", "bob", False),
    ]


def test_regrant_keeps_admin_option(storage):
    _run(storage, "GRANT writers TO bob WITH ADMIN OPTION")
    # A plain re-grant must NOT clear an existing admin option (PG semantics).
    _run(storage, "GRANT writers TO bob")
    assert _run(storage, _MEMBERS_SQL).rows == [("writers", "bob", True)]


def test_revoke_removes_membership(storage):
    _run(storage, "GRANT readers TO alice")
    assert _run(storage, "REVOKE readers FROM alice").command_tag == "REVOKE ROLE"
    assert _run(storage, "SELECT count(*) FROM pg_catalog.pg_auth_members").rows == [(0,)]


def test_revoke_admin_option_for_keeps_membership(storage):
    _run(storage, "GRANT writers TO bob WITH ADMIN OPTION")
    _run(storage, "REVOKE ADMIN OPTION FOR writers FROM bob")
    # Membership stays; only the admin option is cleared.
    assert _run(storage, _MEMBERS_SQL).rows == [("writers", "bob", False)]


def test_privilege_grant_is_not_a_membership(storage):
    _run(storage, "CREATE TABLE t (id int)")
    # GRANT ... ON <table> is a privilege grant, not role membership.
    assert _run(storage, "GRANT SELECT ON t TO alice").command_tag == "GRANT"
    assert _run(storage, "SELECT count(*) FROM pg_catalog.pg_auth_members").rows == [(0,)]


def test_grantor_is_connecting_user(storage):
    _run(storage, "GRANT readers TO alice")
    rows = _run(
        storage,
        "SELECT g.rolname FROM pg_catalog.pg_auth_members am "
        "JOIN pg_catalog.pg_roles g ON g.oid = am.grantor",
    ).rows
    assert rows == [("postgres",)]
