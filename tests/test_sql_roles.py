"""SQL-level roles — ``CREATE ROLE`` / ``CREATE USER`` / ``ALTER ROLE`` /
``DROP ROLE``, ``GRANT`` / ``REVOKE`` (accepted), and ``pg_roles`` reflection.

Roles are recorded in the catalog for reflection and DDL acceptance; they are
distinct from the wire server's SCRAM auth users (constructor config) — a SQL
role does not by itself grant a login credential, and privileges aren't enforced.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


# -- CREATE / DROP / ALTER ----------------------------------------------------- #


def test_create_role(storage, session):
    assert run(storage, session, "CREATE ROLE alice").command_tag == "CREATE ROLE"
    rows = run(storage, session, "SELECT rolname FROM pg_roles WHERE rolname = 'alice'").rows
    assert rows == [("alice",)]


def test_create_user_implies_login(storage, session):
    run(storage, session, "CREATE USER bob WITH PASSWORD 'secret'")
    rows = run(storage, session, "SELECT rolcanlogin FROM pg_roles WHERE rolname = 'bob'").rows
    assert rows == [(True,)]


def test_create_role_attributes(storage, session):
    run(storage, session, "CREATE ROLE carol LOGIN SUPERUSER CREATEDB CREATEROLE")
    rows = run(
        storage,
        session,
        "SELECT rolsuper, rolcanlogin, rolcreatedb, rolcreaterole "
        "FROM pg_roles WHERE rolname = 'carol'",
    ).rows
    assert rows == [(True, True, True, True)]


def test_create_role_negated_attributes(storage, session):
    run(storage, session, "CREATE ROLE dan NOLOGIN NOSUPERUSER")
    rows = run(
        storage, session, "SELECT rolsuper, rolcanlogin FROM pg_roles WHERE rolname = 'dan'"
    ).rows
    assert rows == [(False, False)]


def test_duplicate_role_rejected(storage, session):
    run(storage, session, "CREATE ROLE alice")
    assert sqlstate(storage, session, "CREATE ROLE alice") == "42710"


def test_alter_role_updates_attributes(storage, session):
    run(storage, session, "CREATE ROLE alice")
    run(storage, session, "ALTER ROLE alice WITH LOGIN CREATEDB")
    rows = run(
        storage, session, "SELECT rolcanlogin, rolcreatedb FROM pg_roles WHERE rolname = 'alice'"
    ).rows
    assert rows == [(True, True)]


def test_alter_missing_role_errors(storage, session):
    assert sqlstate(storage, session, "ALTER ROLE nope WITH LOGIN") == "42704"


def test_drop_role(storage, session):
    run(storage, session, "CREATE ROLE alice")
    assert run(storage, session, "DROP ROLE alice").command_tag == "DROP ROLE"
    assert run(storage, session, "SELECT rolname FROM pg_roles WHERE rolname = 'alice'").rows == []


def test_drop_missing_role_errors(storage, session):
    assert sqlstate(storage, session, "DROP ROLE nope") == "42704"


def test_drop_role_if_exists(storage, session):
    assert run(storage, session, "DROP ROLE IF EXISTS nope").command_tag == "DROP ROLE"


def test_quoted_role_name(storage, session):
    run(storage, session, 'CREATE ROLE "Weird Name"')
    rows = run(storage, session, "SELECT rolname FROM pg_roles WHERE rolname = 'Weird Name'").rows
    assert rows == [("Weird Name",)]


# -- GRANT / REVOKE (accepted no-ops) ------------------------------------------ #


def test_grant_privilege_accepted(storage, session):
    run(storage, session, "CREATE ROLE alice")
    run(storage, session, "CREATE TABLE t (id bigint PRIMARY KEY)")
    assert run(storage, session, "GRANT SELECT ON t TO alice").command_tag == "GRANT"


def test_revoke_privilege_accepted(storage, session):
    run(storage, session, "CREATE ROLE alice")
    run(storage, session, "CREATE TABLE t (id bigint PRIMARY KEY)")
    assert run(storage, session, "REVOKE SELECT ON t FROM alice").command_tag == "REVOKE"


def test_grant_role_membership_accepted(storage, session):
    # Role membership (GRANT <role> TO <member>) is recorded and tagged GRANT
    # ROLE / REVOKE ROLE (#138); see test_sql_role_membership.py for the
    # pg_auth_members reflection over the real Storage.
    run(storage, session, "CREATE ROLE alice")
    run(storage, session, "CREATE ROLE bob")
    assert run(storage, session, "GRANT alice TO bob").command_tag == "GRANT ROLE"
    assert run(storage, session, "REVOKE alice FROM bob").command_tag == "REVOKE ROLE"


# -- reflection ---------------------------------------------------------------- #


def test_connection_user_is_a_role(storage, session):
    """The connecting user always shows up as a superuser login role, even with no
    explicit CREATE ROLE (like Postgres' bootstrap superuser)."""
    rows = run(
        storage,
        session,
        "SELECT rolsuper, rolcanlogin FROM pg_roles WHERE rolname = 'secantus'",
    ).rows
    assert rows == [(True, True)]


def test_pg_roles_lists_all(storage, session):
    run(storage, session, "CREATE ROLE alice")
    run(storage, session, "CREATE ROLE bob")
    rows = run(storage, session, "SELECT rolname FROM pg_roles ORDER BY rolname").rows
    assert ("alice",) in rows and ("bob",) in rows and ("secantus",) in rows
