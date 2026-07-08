"""Per-statement RBAC authorization for the SQL server (#193).

The SQL surface reuses the Mongo RBAC engine (``secantus.rbac``): each statement
maps to one action on the connection's database and is gated by
``check_privilege`` against the session's role bindings. Authorization is opt-in
— a session only enforces it when ``authz_active`` is set (the wire server does
that when started with ``require_auth`` + per-user roles). The embedded
``run_sql`` API and trust mode leave it off, so the surface stays unrestricted
there. These tests drive ``run_sql`` over a real ``Storage`` with hand-built
sessions; the over-the-wire path is covered in ``test_pgserver_auth.py``.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


def _admin_session() -> Session:
    """An unrestricted session (authz off) — used to seed the schema/data."""
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    sess = _admin_session()
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int)", session=sess)
    run_sql(s, DB, "INSERT INTO t (id, n) VALUES (1, 10)", session=sess)
    try:
        yield s
    finally:
        s.close()


def gated(roles, *, database: str = DB) -> Session:
    return Session(database=database, user="joe", authz_active=True, roles=list(roles))


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)


def _denied(storage, session, sql) -> SQLError:
    with pytest.raises(SQLError) as ei:
        _run(storage, session, sql)
    assert ei.value.sqlstate == "42501", f"expected 42501, got {ei.value.sqlstate}"
    return ei.value


# --------------------------------------------------------------------------- #
# Opt-in: inactive sessions (embedded API / trust mode) stay unrestricted.
# --------------------------------------------------------------------------- #


def test_inactive_session_is_unrestricted(storage):
    """A session with authz off (the embedded default) runs anything, even with
    no roles — preserving prior behaviour."""
    sess = Session(database=DB)  # authz_active defaults False, roles empty
    assert _run(storage, sess, "SELECT n FROM t")[0].rows == [(10,)]
    _run(storage, sess, "INSERT INTO t (id, n) VALUES (2, 20)")
    _run(storage, sess, "DROP TABLE t")  # even DDL is allowed


# --------------------------------------------------------------------------- #
# Built-in roles gate reads / writes / DDL by database.
# --------------------------------------------------------------------------- #


def test_read_role_allows_select_denies_writes(storage):
    sess = gated([{"role": "read", "db": DB}])
    assert _run(storage, sess, "SELECT n FROM t")[0].rows == [(10,)]
    _denied(storage, sess, "INSERT INTO t (id, n) VALUES (2, 20)")
    _denied(storage, sess, "UPDATE t SET n = 99 WHERE id = 1")
    _denied(storage, sess, "DELETE FROM t WHERE id = 1")


def test_readwrite_role_allows_dml(storage):
    sess = gated([{"role": "readWrite", "db": DB}])
    _run(storage, sess, "INSERT INTO t (id, n) VALUES (2, 20)")
    _run(storage, sess, "UPDATE t SET n = 99 WHERE id = 2")
    assert _run(storage, sess, "SELECT n FROM t WHERE id = 2")[0].rows == [(99,)]
    _run(storage, sess, "DELETE FROM t WHERE id = 2")


def test_no_roles_denies_data_but_allows_session_statements(storage):
    """An authenticated user with no grants can connect and run session-only
    statements, but touches no data (real RBAC: LOGIN without privileges)."""
    sess = gated([])
    _denied(storage, sess, "SELECT n FROM t")
    # Transaction control and SET carry no data privilege.
    assert _run(storage, sess, "BEGIN")[0].command_tag == "BEGIN"
    _denied(storage, sess, "SELECT n FROM t")  # still denied inside the txn
    assert _run(storage, sess, "ROLLBACK")[0].command_tag == "ROLLBACK"
    assert _run(storage, sess, "SET search_path TO public")[0].command_tag == "SET"


def test_role_binding_is_scoped_to_its_database(storage):
    """A role granted on another db doesn't authorize this connection's db."""
    sess = gated([{"role": "readWrite", "db": "other"}])
    _denied(storage, sess, "SELECT n FROM t")


def test_ddl_requires_more_than_read(storage):
    reader = gated([{"role": "read", "db": DB}])
    _denied(storage, reader, "CREATE TABLE t2 (a int)")
    _denied(storage, reader, "DROP TABLE t")
    writer = gated([{"role": "readWrite", "db": DB}])
    _run(storage, writer, "CREATE TABLE t2 (a int)")
    _run(storage, writer, "DROP TABLE t2")


def test_root_allows_everything(storage):
    sess = gated([{"role": "root", "db": "admin"}])
    assert _run(storage, sess, "SELECT n FROM t")[0].rows == [(10,)]
    _run(storage, sess, "INSERT INTO t (id, n) VALUES (3, 30)")
    _run(storage, sess, "CREATE TABLE t3 (a int)")
    _run(storage, sess, "DROP TABLE t3")


# --------------------------------------------------------------------------- #
# Statement-shape edge cases.
# --------------------------------------------------------------------------- #


def test_data_modifying_cte_needs_write(storage):
    """A read-only role can't smuggle a write through a data-modifying CTE whose
    top node parses as a SELECT."""
    sess = gated([{"role": "read", "db": DB}])
    _denied(storage, sess, "WITH x AS (DELETE FROM t RETURNING id) SELECT * FROM x")


def test_declare_cursor_needs_read(storage):
    reader = gated([{"role": "read", "db": DB}])
    assert (
        _run(storage, reader, "DECLARE c CURSOR FOR SELECT id FROM t")[0].command_tag
        == "DECLARE CURSOR"
    )
    none = gated([])
    _denied(storage, none, "DECLARE c CURSOR FOR SELECT id FROM t")


def test_role_and_grant_commands_need_user_admin(storage):
    reader = gated([{"role": "read", "db": DB}])
    _denied(storage, reader, "CREATE ROLE analyst")
    _denied(storage, reader, "GRANT SELECT ON t TO analyst")


# --------------------------------------------------------------------------- #
# Custom (non-built-in) roles resolve through the real Storage.get_role.
# --------------------------------------------------------------------------- #


def test_custom_role_resolves_through_storage(storage):
    # A custom ``appReader`` role granting only ``find`` on db ``app``, persisted
    # in the real roles table so the authz gate resolves it via Storage.get_role.
    storage.add_role(
        "app",
        "appReader",
        {
            "role": "appReader",
            "db": "app",
            "privileges": [{"resource": {"db": "app", "collection": ""}, "actions": ["find"]}],
            "roles": [],
        },
    )
    sess = gated([{"role": "appReader", "db": "app"}])
    assert _run(storage, sess, "SELECT n FROM t")[0].rows == [(10,)]
    _denied(storage, sess, "INSERT INTO t (id, n) VALUES (2, 20)")
