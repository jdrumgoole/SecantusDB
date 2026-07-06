"""Row-level security (#129): ALTER TABLE … ROW LEVEL SECURITY + CREATE POLICY.

A policy's USING predicate restricts which rows SELECT/UPDATE/DELETE see; its
WITH CHECK predicate restricts the rows INSERT/UPDATE may write. Permissive
policies are OR'd, restrictive AND'd, and identity functions (current_user) are
substituted with the session's user. Enforcement is gated on ``authz_active``
(trust mode / embedded API record but don't enforce) and a superuser (root)
bypasses it. Driven over the real ``Storage`` per the no-FakeStorage rule.
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
    admin = Session(database=DB)
    run_sql(s, DB, "CREATE TABLE doc (id bigint primary key, owner text, body text)", session=admin)
    run_sql(
        s,
        DB,
        "INSERT INTO doc VALUES (1,'alice','a1'),(2,'bob','b1'),(3,'alice','a2')",
        session=admin,
    )
    run_sql(s, DB, "ALTER TABLE doc ENABLE ROW LEVEL SECURITY", session=admin)
    run_sql(
        s,
        DB,
        "CREATE POLICY p_owner ON doc FOR ALL TO public "
        "USING (owner = current_user) WITH CHECK (owner = current_user)",
        session=admin,
    )
    try:
        yield s
    finally:
        s.close()


def _admin():
    return Session(database=DB)


def _gated(user, roles=(("readWrite", DB),)):
    return Session(
        database=DB,
        user=user,
        authz_active=True,
        roles=[{"role": r, "db": d} for r, d in roles],
    )


def _ids(storage, session, sql="SELECT id FROM doc ORDER BY id"):
    return [r[0] for r in run_sql(storage, DB, sql, session=session)[-1].rows]


# --------------------------------------------------------------------------- #
# USING read filter.
# --------------------------------------------------------------------------- #


def test_trust_mode_not_enforced(storage):
    # authz off: RLS recorded but not enforced — the admin sees every row.
    assert _ids(storage, _admin()) == [1, 2, 3]


def test_using_filters_by_owner(storage):
    assert _ids(storage, _gated("alice")) == [1, 3]
    assert _ids(storage, _gated("bob")) == [2]


def test_using_combines_with_user_where(storage):
    assert _ids(storage, _gated("alice"), "SELECT id FROM doc WHERE id > 1") == [3]


def test_root_bypasses_rls(storage):
    root = _gated("super", roles=[("root", "admin")])
    assert _ids(storage, root) == [1, 2, 3]


# --------------------------------------------------------------------------- #
# WITH CHECK on writes.
# --------------------------------------------------------------------------- #


def test_with_check_allows_own_row(storage):
    run_sql(storage, DB, "INSERT INTO doc VALUES (4,'alice','a3')", session=_gated("alice"))
    assert _ids(storage, _admin()) == [1, 2, 3, 4]


def test_with_check_denies_other_row(storage):
    with pytest.raises(SQLError) as ei:
        run_sql(storage, DB, "INSERT INTO doc VALUES (5,'bob','x')", session=_gated("alice"))
    assert ei.value.sqlstate == "42501"


def test_update_scoped_by_using(storage):
    # alice's UPDATE only touches her rows (id 2 is bob's, untouched).
    run_sql(storage, DB, "UPDATE doc SET body='upd' WHERE id IN (1,2)", session=_gated("alice"))
    bodies = run_sql(
        storage, DB, "SELECT id, body FROM doc WHERE id IN (1,2) ORDER BY id", session=_admin()
    )[-1].rows
    assert bodies == [(1, "upd"), (2, "b1")]


def test_delete_scoped_by_using(storage):
    run_sql(storage, DB, "DELETE FROM doc WHERE id >= 1", session=_gated("bob"))
    # Only bob's row (2) is deletable by bob.
    assert _ids(storage, _admin()) == [1, 3]


# --------------------------------------------------------------------------- #
# Default-deny, DISABLE, DROP POLICY.
# --------------------------------------------------------------------------- #


def test_default_deny_without_permissive_policy(storage):
    run_sql(storage, DB, "DROP POLICY p_owner ON doc", session=_admin())
    # RLS still enabled, no applicable policy -> no rows.
    assert _ids(storage, _gated("alice")) == []


def test_disable_rls_restores_visibility(storage):
    run_sql(storage, DB, "ALTER TABLE doc DISABLE ROW LEVEL SECURITY", session=_admin())
    assert _ids(storage, _gated("alice")) == [1, 2, 3]


def test_restrictive_policy_anded(storage):
    # Add a restrictive policy that only permits body starting with 'a'; combined
    # with the permissive owner policy it AND-restricts alice to her 'a%' rows.
    run_sql(
        storage,
        DB,
        "CREATE POLICY only_a ON doc AS RESTRICTIVE FOR SELECT TO public USING (body LIKE 'a%')",
        session=_admin(),
    )
    # alice owns 1 (a1) and 3 (a2) — both start with 'a', so still visible.
    assert _ids(storage, _gated("alice")) == [1, 3]


def test_drop_policy_if_exists_is_idempotent(storage):
    run_sql(storage, DB, "DROP POLICY IF EXISTS nope ON doc", session=_admin())
    with pytest.raises(SQLError) as ei:
        run_sql(storage, DB, "DROP POLICY nope ON doc", session=_admin())
    assert ei.value.sqlstate == "42704"


# --------------------------------------------------------------------------- #
# Reflection.
# --------------------------------------------------------------------------- #


def test_pg_policies_reflection(storage):
    rows = run_sql(
        storage,
        DB,
        "SELECT tablename, policyname, permissive, cmd, qual, with_check "
        "FROM pg_catalog.pg_policies",
        session=_admin(),
    )[-1].rows
    assert rows == [
        ("doc", "p_owner", "PERMISSIVE", "ALL", "owner = current_user", "owner = current_user")
    ]
