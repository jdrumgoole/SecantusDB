"""Enforced table-level GRANT / REVOKE privileges (#127).

``GRANT``/``REVOKE`` on a table for SELECT/INSERT/UPDATE/DELETE are persisted and
enforced as an *additive* layer over the Mongo RBAC roles: a table grant lets a
user run an operation their role wouldn't otherwise cover, and a REVOKE takes
that back. Enforcement only bites when the session marks authorization active
(the wire server with require_auth + roles) — the embedded ``run_sql`` API and
trust mode stay unrestricted, so grants there are recorded and reflected but not
enforced. Grants surface via ``information_schema.role_table_grants`` /
``table_privileges`` and ``has_table_privilege()``.

Driven over the real ``Storage`` (per the project rule against ``FakeStorage`` in
new tests); the over-the-wire path is covered in ``test_pgserver_pg8000.py``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    admin = Session(database=DB)  # authz off — seeds schema/data + issues grants
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int)", session=admin)
    run_sql(s, DB, "INSERT INTO t (id, n) VALUES (1, 10)", session=admin)
    try:
        yield s
    finally:
        s.close()


def _admin() -> Session:
    return Session(database=DB)


def gated(user: str, roles=()) -> Session:
    return Session(database=DB, user=user, authz_active=True, roles=list(roles))


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)


def _rows(storage, session, sql):
    return _run(storage, session, sql)[-1].rows


def _denied(storage, session, sql) -> SQLError:
    with pytest.raises(SQLError) as ei:
        _run(storage, session, sql)
    assert ei.value.sqlstate == "42501", f"expected 42501, got {ei.value.sqlstate}"
    return ei.value


# --------------------------------------------------------------------------- #
# GRANT/REVOKE persist + reflect.
# --------------------------------------------------------------------------- #


def test_grant_persists_and_reflects(storage):
    _run(storage, _admin(), "GRANT SELECT, INSERT ON t TO alice")
    rows = _rows(
        storage,
        _admin(),
        "SELECT grantee, privilege_type FROM information_schema.role_table_grants "
        "ORDER BY grantee, privilege_type",
    )
    assert rows == [("alice", "INSERT"), ("alice", "SELECT")]


def test_grant_all_expands_to_every_privilege(storage):
    _run(storage, _admin(), "GRANT ALL ON t TO alice")
    rows = _rows(
        storage,
        _admin(),
        "SELECT privilege_type FROM information_schema.table_privileges "
        "WHERE grantee = 'alice' ORDER BY privilege_type",
    )
    # ALL expands to PG's seven table privileges.
    assert set(r[0] for r in rows) == {
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "TRUNCATE",
        "REFERENCES",
        "TRIGGER",
    }


def test_revoke_removes_and_prunes(storage):
    _run(storage, _admin(), "GRANT SELECT, INSERT ON t TO alice")
    _run(storage, _admin(), "REVOKE INSERT ON t FROM alice")
    assert _rows(
        storage,
        _admin(),
        "SELECT privilege_type FROM information_schema.role_table_grants WHERE grantee = 'alice'",
    ) == [("SELECT",)]
    # Revoking the last privilege drops the grant row entirely.
    _run(storage, _admin(), "REVOKE SELECT ON t FROM alice")
    assert (
        _rows(
            storage,
            _admin(),
            "SELECT privilege_type FROM information_schema.role_table_grants "
            "WHERE grantee = 'alice'",
        )
        == []
    )


def test_grant_on_unknown_table_errors(storage):
    with pytest.raises(SQLError) as ei:
        _run(storage, _admin(), "GRANT SELECT ON nope TO alice")
    assert ei.value.sqlstate == "42P01"


# --------------------------------------------------------------------------- #
# has_table_privilege().
# --------------------------------------------------------------------------- #


def test_has_table_privilege_three_and_two_arg(storage):
    _run(storage, _admin(), "GRANT SELECT ON t TO alice")
    assert _rows(storage, _admin(), "SELECT has_table_privilege('alice', 't', 'SELECT')") == [
        (True,)
    ]
    assert _rows(storage, _admin(), "SELECT has_table_privilege('alice', 't', 'INSERT')") == [
        (False,)
    ]
    # WITH GRANT OPTION suffix on the privilege string is tolerated.
    assert _rows(
        storage, _admin(), "SELECT has_table_privilege('alice', 't', 'SELECT WITH GRANT OPTION')"
    ) == [(True,)]
    # Two-arg form checks the session user (default 'secantus', no grant).
    assert _rows(storage, _admin(), "SELECT has_table_privilege('t', 'SELECT')") == [(False,)]


def test_has_table_privilege_public(storage):
    _run(storage, _admin(), "GRANT SELECT ON t TO PUBLIC")
    # Any user inherits a PUBLIC grant.
    assert _rows(storage, _admin(), "SELECT has_table_privilege('nobody', 't', 'SELECT')") == [
        (True,)
    ]


# --------------------------------------------------------------------------- #
# Enforcement (additive over Mongo RBAC), gated on authz_active.
# --------------------------------------------------------------------------- #


def test_table_grant_authorizes_a_roleless_user(storage):
    _run(storage, _admin(), "GRANT SELECT ON t TO alice")
    alice = gated("alice")  # no Mongo role at all
    assert _rows(storage, alice, "SELECT n FROM t") == [(10,)]
    _denied(storage, alice, "INSERT INTO t (id, n) VALUES (2, 20)")
    # Grant the write, and it goes through.
    _run(storage, _admin(), "GRANT INSERT ON t TO alice")
    _run(storage, alice, "INSERT INTO t (id, n) VALUES (2, 20)")
    assert _rows(storage, alice, "SELECT n FROM t WHERE id = 2") == [(20,)]


def test_revoke_takes_access_back(storage):
    _run(storage, _admin(), "GRANT SELECT ON t TO alice")
    alice = gated("alice")
    assert _rows(storage, alice, "SELECT n FROM t") == [(10,)]
    _run(storage, _admin(), "REVOKE SELECT ON t FROM alice")
    _denied(storage, alice, "SELECT n FROM t")


def test_public_grant_authorizes_everyone(storage):
    _run(storage, _admin(), "GRANT SELECT ON t TO PUBLIC")
    assert _rows(storage, gated("anyone"), "SELECT n FROM t") == [(10,)]


def test_grant_matched_by_role_name(storage):
    # A grant to a role the user holds (not the user name) authorizes them.
    _run(storage, _admin(), "GRANT SELECT ON t TO analyst")
    sess = gated("joe", [{"role": "analyst", "db": DB}])
    assert _rows(storage, sess, "SELECT n FROM t") == [(10,)]


def test_grant_is_additive_not_restrictive(storage):
    # A Mongo readWrite role already covers writes; a table grant/revoke can't
    # shrink that (the two axes are additive — documented behaviour).
    rw = gated("carol", [{"role": "readWrite", "db": DB}])
    _run(storage, _admin(), "GRANT SELECT ON t TO PUBLIC")  # unrelated
    _run(storage, rw, "INSERT INTO t (id, n) VALUES (3, 30)")
    assert _rows(storage, rw, "SELECT n FROM t WHERE id = 3") == [(30,)]


def test_embedded_api_ignores_grants(storage):
    # authz off (embedded default): grants are recorded but never enforced.
    _run(storage, _admin(), "GRANT SELECT ON t TO alice")
    plain = Session(database=DB)  # authz_active False
    assert _rows(storage, plain, "SELECT n FROM t") == [(10,)]
    _run(storage, plain, "INSERT INTO t (id, n) VALUES (9, 90)")  # write allowed


class TestRelaclReflection:
    """pg_class.relacl / relowner as pgjdbc's getTablePrivileges reads them:
    an untouched relation reports NULL (owner holds everything implicitly), and
    the owner's role oid must match pg_roles so the metadata join resolves."""

    def _priv_join(self, storage, session, relname):
        return run_sql(
            storage,
            DB,
            "SELECT r.rolname, c.relacl FROM pg_catalog.pg_namespace n,"
            " pg_catalog.pg_class c, pg_catalog.pg_roles r"
            " WHERE c.relnamespace = n.oid AND c.relowner = r.oid"
            f" AND c.relkind IN ('r','p','v','m','f') AND c.relname LIKE '{relname}'",
            session=session,
        )[-1].rows

    def test_untouched_relation_join_returns_owner_null_acl(self, storage):
        s = Session(database=DB, user="alice")
        run_sql(storage, DB, "CREATE TABLE owned (a int)", session=s)
        assert self._priv_join(storage, s, "owned") == [("alice", None)]

    def test_view_owner_join_resolves(self, storage):
        s = Session(database=DB, user="alice")
        run_sql(storage, DB, "CREATE VIEW ov AS SELECT id FROM t", session=s)
        assert self._priv_join(storage, s, "ov") == [("alice", None)]

    def test_revoke_all_from_owner_empties_acl(self, storage):
        s = Session(database=DB, user="alice")
        run_sql(storage, DB, "CREATE TABLE owned (a int)", session=s)
        run_sql(storage, DB, "REVOKE ALL ON owned FROM PUBLIC", session=s)
        run_sql(storage, DB, "REVOKE ALL ON owned FROM alice", session=s)
        # materialized to an empty aclitem array — pgjdbc reads no owner SELECT
        assert self._priv_join(storage, s, "owned") == [("alice", "{}")]

    def test_grant_to_third_party_materializes_owner_and_grantee(self, storage):
        s = Session(database=DB, user="alice")
        run_sql(storage, DB, "CREATE TABLE owned (a int)", session=s)
        run_sql(storage, DB, "GRANT SELECT ON owned TO bob", session=s)
        (rolname, acl) = self._priv_join(storage, s, "owned")[0]
        assert rolname == "alice"
        assert acl == "{alice=arwdDxt/alice,bob=r/alice}"
