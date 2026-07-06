"""SET ROLE / SET SESSION AUTHORIZATION (#128).

``SET ROLE`` changes the session's *current role* (``current_user`` /
``current_role`` / ``user``) while leaving the *session user* (``session_user``)
unchanged; ``SET SESSION AUTHORIZATION`` changes both. ``RESET`` restores the
login identity. The current role is what the #127 table-grant gate matches, so a
session can SET ROLE to a role it holds and pick up that role's grants. When
authorization is active, a session can't borrow an identity it doesn't hold
(escalation guard).

Driven over the real ``Storage`` (per the no-FakeStorage rule for new tests).
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
    run_sql(s, DB, "CREATE TABLE t (id bigint primary key, n int)", session=admin)
    run_sql(s, DB, "INSERT INTO t (id, n) VALUES (1, 10)", session=admin)
    run_sql(s, DB, "GRANT SELECT ON t TO analyst", session=admin)
    try:
        yield s
    finally:
        s.close()


def _val(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows[0][0]


def _run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)


# --------------------------------------------------------------------------- #
# current_user / session_user / synonyms resolve.
# --------------------------------------------------------------------------- #


def test_identity_functions_resolve(storage):
    s = Session(database=DB, user="joe")
    assert _val(storage, s, "SELECT current_user") == "joe"
    assert _val(storage, s, "SELECT session_user") == "joe"
    assert _val(storage, s, "SELECT current_role") == "joe"
    assert _val(storage, s, "SELECT user") == "joe"


# --------------------------------------------------------------------------- #
# SET ROLE.
# --------------------------------------------------------------------------- #


def test_set_role_changes_current_user_not_session_user(storage):
    s = Session(database=DB, user="joe")
    assert _run(storage, s, "SET ROLE analyst")[0].command_tag == "SET"
    assert _val(storage, s, "SELECT current_user") == "analyst"
    assert _val(storage, s, "SELECT session_user") == "joe"  # session user unchanged
    assert _val(storage, s, "SHOW role") == "analyst"


def test_reset_role_restores(storage):
    s = Session(database=DB, user="joe")
    _run(storage, s, "SET ROLE analyst")
    _run(storage, s, "RESET ROLE")
    assert _val(storage, s, "SELECT current_user") == "joe"
    # SET ROLE NONE is equivalent to RESET ROLE.
    _run(storage, s, "SET ROLE analyst")
    _run(storage, s, "SET ROLE NONE")
    assert _val(storage, s, "SELECT current_user") == "joe"


def test_set_role_quoted_identifier(storage):
    s = Session(database=DB, user="joe")
    _run(storage, s, "SET ROLE 'analyst'")
    assert _val(storage, s, "SELECT current_user") == "analyst"


# --------------------------------------------------------------------------- #
# SET SESSION AUTHORIZATION.
# --------------------------------------------------------------------------- #


def test_set_session_authorization_changes_both(storage):
    s = Session(database=DB, user="joe")
    _run(storage, s, "SET SESSION AUTHORIZATION alice")
    assert _val(storage, s, "SELECT current_user") == "alice"
    assert _val(storage, s, "SELECT session_user") == "alice"
    _run(storage, s, "RESET SESSION AUTHORIZATION")
    assert _val(storage, s, "SELECT current_user") == "joe"
    assert _val(storage, s, "SELECT session_user") == "joe"


def test_session_authorization_resets_current_role(storage):
    s = Session(database=DB, user="joe")
    _run(storage, s, "SET ROLE analyst")
    _run(storage, s, "SET SESSION AUTHORIZATION alice")
    # A new session user resets the current role to the new session user.
    assert _val(storage, s, "SELECT current_user") == "alice"


# --------------------------------------------------------------------------- #
# Integration with #127 table-grant enforcement.
# --------------------------------------------------------------------------- #


def _gated(user, roles=()):
    return Session(database=DB, user=user, authz_active=True, roles=list(roles))


def test_set_role_picks_up_that_roles_grants(storage):
    # bob holds the analyst role (Mongo binding) but no read privilege; SET ROLE
    # analyst makes the analyst table grant apply.
    bob = _gated("bob", [{"role": "analyst", "db": DB}])
    _run(storage, bob, "SET ROLE analyst")
    assert run_sql(storage, DB, "SELECT n FROM t", session=bob)[-1].rows == [(10,)]


def test_escalation_guard_denies_unheld_identity(storage):
    eve = _gated("eve", [{"role": "read", "db": DB}])
    with pytest.raises(SQLError) as ei:
        _run(storage, eve, "SET SESSION AUTHORIZATION admin")
    assert ei.value.sqlstate == "42501"
    with pytest.raises(SQLError) as ei2:
        _run(storage, eve, "SET ROLE analyst")  # eve doesn't hold analyst
    assert ei2.value.sqlstate == "42501"


def test_root_may_assume_any_identity(storage):
    root = _gated("super", [{"role": "root", "db": "admin"}])
    _run(storage, root, "SET ROLE analyst")
    assert _val(storage, root, "SELECT current_user") == "analyst"
    _run(storage, root, "SET SESSION AUTHORIZATION alice")
    assert _val(storage, root, "SELECT session_user") == "alice"


def test_trust_mode_allows_any_set_role(storage):
    # authz off: SET ROLE / SET SESSION AUTHORIZATION accept any identity.
    s = Session(database=DB, user="joe")  # authz_active False
    _run(storage, s, "SET ROLE anybody")
    assert _val(storage, s, "SELECT current_user") == "anybody"
    _run(storage, s, "SET SESSION AUTHORIZATION whoever")
    assert _val(storage, s, "SELECT session_user") == "whoever"
