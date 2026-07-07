"""Column-level privileges (#131): GRANT SELECT (col) ON t.

Finer-grained than the table-level grants of #127: a column grant authorizes a
role for exactly the named columns. Enforcement is additive over the Mongo RBAC
gate and table grants (a role or table grant already covering the action wins);
a column grant lets a user run a statement only when *every* column it touches is
granted. Surfaced via information_schema.column_privileges and
has_column_privilege(). Driven over the real ``Storage``.
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
    run_sql(
        s, DB, "CREATE TABLE t (id bigint primary key, a int, b int, secret text)", session=admin
    )
    run_sql(s, DB, "INSERT INTO t VALUES (1, 10, 20, 'x')", session=admin)
    run_sql(s, DB, "GRANT SELECT (id, a) ON t TO alice", session=admin)
    run_sql(s, DB, "GRANT UPDATE (a) ON t TO alice", session=admin)
    try:
        yield s
    finally:
        s.close()


def _admin():
    return Session(database=DB)


def _gated(user, roles=()):
    return Session(database=DB, user=user, authz_active=True, roles=list(roles))


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)


def _rows(storage, session, sql):
    return _run(storage, session, sql)[-1].rows


def _denied(storage, session, sql):
    with pytest.raises(SQLError) as ei:
        _run(storage, session, sql)
    assert ei.value.sqlstate == "42501", f"expected 42501, got {ei.value.sqlstate}"


# --------------------------------------------------------------------------- #
# SELECT column enforcement.
# --------------------------------------------------------------------------- #


def test_select_granted_columns(storage):
    assert _rows(storage, _gated("alice"), "SELECT id, a FROM t") == [(1, 10)]


def test_select_ungranted_column_denied(storage):
    _denied(storage, _gated("alice"), "SELECT id, secret FROM t")
    _denied(storage, _gated("alice"), "SELECT b FROM t")


def test_select_star_denied_without_all_columns(storage):
    # SELECT * touches every column; alice lacks secret / b.
    _denied(storage, _gated("alice"), "SELECT * FROM t")


def test_where_columns_need_privilege(storage):
    # A WHERE reference to an ungranted column is denied (PG requires SELECT there).
    _denied(storage, _gated("alice"), "SELECT id FROM t WHERE secret = 'x'")
    # A WHERE over a granted column is fine.
    assert _rows(storage, _gated("alice"), "SELECT id FROM t WHERE a = 10") == [(1,)]


# --------------------------------------------------------------------------- #
# UPDATE column enforcement.
# --------------------------------------------------------------------------- #


def test_update_granted_column(storage):
    _run(storage, _gated("alice"), "UPDATE t SET a = 99 WHERE id = 1")
    assert _rows(storage, _admin(), "SELECT a FROM t WHERE id = 1") == [(99,)]


def test_update_ungranted_column_denied(storage):
    _denied(storage, _gated("alice"), "UPDATE t SET b = 5 WHERE id = 1")


# --------------------------------------------------------------------------- #
# REVOKE + additivity.
# --------------------------------------------------------------------------- #


def test_revoke_column_privilege(storage):
    run_sql(storage, DB, "REVOKE SELECT (a) ON t FROM alice", session=_admin())
    _denied(storage, _gated("alice"), "SELECT id, a FROM t")
    # id is still granted.
    assert _rows(storage, _gated("alice"), "SELECT id FROM t") == [(1,)]


def test_table_grant_still_covers_all_columns(storage):
    # A whole-table SELECT grant lets bob read any column (column grants are a
    # finer *addition*, they don't shrink a table grant).
    run_sql(storage, DB, "GRANT SELECT ON t TO bob", session=_admin())
    assert _rows(storage, _gated("bob"), "SELECT id, a, b, secret FROM t") == [(1, 10, 20, "x")]


def test_role_holder_unaffected(storage):
    # A readWrite Mongo role passes the RBAC gate before column checks apply.
    carol = _gated("carol", [{"role": "readWrite", "db": DB}])
    assert _rows(storage, carol, "SELECT id, a, b, secret FROM t") == [(1, 10, 20, "x")]


# --------------------------------------------------------------------------- #
# Reflection.
# --------------------------------------------------------------------------- #


def test_column_privileges_reflection(storage):
    rows = _rows(
        storage,
        _admin(),
        "SELECT grantee, column_name, privilege_type FROM information_schema.column_privileges "
        "ORDER BY column_name, privilege_type",
    )
    assert rows == [
        ("alice", "a", "SELECT"),
        ("alice", "a", "UPDATE"),
        ("alice", "id", "SELECT"),
    ]


def test_has_column_privilege(storage):
    assert _rows(storage, _admin(), "SELECT has_column_privilege('alice', 't', 'a', 'SELECT')") == [
        (True,)
    ]
    assert _rows(
        storage, _admin(), "SELECT has_column_privilege('alice', 't', 'secret', 'SELECT')"
    ) == [(False,)]
    # A whole-table grant satisfies has_column_privilege for any column.
    run_sql(storage, DB, "GRANT SELECT ON t TO dave", session=_admin())
    assert _rows(
        storage, _admin(), "SELECT has_column_privilege('dave', 't', 'secret', 'SELECT')"
    ) == [(True,)]
