"""``INSERT … ON CONFLICT`` — upsert against a conflict target.

``DO NOTHING`` (with or without a conflict target) skips a colliding row;
``DO UPDATE SET … [WHERE …]`` updates the existing row, with ``EXCLUDED.<col>``
bound to the proposed insert row and bare / target-qualified columns to the
existing row. The command tag counts rows inserted *or* updated; skipped rows
don't count. ``RETURNING`` projects the inserted and updated rows.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE t (id bigint primary key, n int, tag text)")
    s.q("INSERT INTO t (id, n, tag) VALUES (1, 5, 'a')")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_do_nothing_on_conflict_skips(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n, tag) VALUES (1, 99, 'z') ON CONFLICT (id) DO NOTHING",
    )
    assert res.command_tag == "INSERT 0 0"
    assert q(storage, session, "SELECT id, n, tag FROM t ORDER BY id").rows == [(1, 5, "a")]


def test_do_nothing_clean_insert(storage, session):
    res = q(storage, session, "INSERT INTO t (id, n) VALUES (2, 7) ON CONFLICT (id) DO NOTHING")
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 5), (2, 7)]


def test_do_nothing_no_target_absorbs_pk_collision(storage, session):
    # A bare ON CONFLICT DO NOTHING (no target) swallows any unique collision.
    res = q(storage, session, "INSERT INTO t (id, n) VALUES (1, 42) ON CONFLICT DO NOTHING")
    assert res.command_tag == "INSERT 0 0"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 5)]


def test_do_update_with_excluded(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n, tag) VALUES (1, 99, 'z') "
        "ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n, tag = EXCLUDED.tag",
    )
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, n, tag FROM t ORDER BY id").rows == [(1, 99, "z")]


def test_do_update_arithmetic_existing_plus_excluded(storage, session):
    # n = t.n + EXCLUDED.n -> 5 + 10 = 15 (existing-row column vs proposed value).
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 10) ON CONFLICT (id) DO UPDATE SET n = t.n + EXCLUDED.n",
    )
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 15)]


def test_do_update_constant(storage, session):
    q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 0) ON CONFLICT (id) DO UPDATE SET n = 100",
    )
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 100)]


def test_do_update_where_gate_blocks(storage, session):
    # WHERE t.n < 5 is false (existing n = 5) -> update skipped, row untouched.
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 1) ON CONFLICT (id) DO UPDATE SET n = 0 WHERE t.n < 5",
    )
    assert res.command_tag == "INSERT 0 0"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 5)]


def test_do_update_where_gate_passes(storage, session):
    # WHERE t.n < 10 is true (existing n = 5) -> update applies.
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 1) ON CONFLICT (id) DO UPDATE SET n = 0 WHERE t.n < 10",
    )
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 0)]


def test_do_update_clean_insert(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (3, 9) ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n",
    )
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 5), (3, 9)]


def test_do_update_returning_projects_updated_row(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 7) "
        "ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n RETURNING id, n",
    )
    assert res.rows == [(1, 7)]


def test_do_nothing_returning_skips_conflict_row(storage, session):
    # id=2 is a clean insert (returned); id=1 conflicts and is skipped (not returned).
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 8), (2, 9) ON CONFLICT (id) DO NOTHING RETURNING id, n",
    )
    assert res.command_tag == "INSERT 0 1"
    assert res.rows == [(2, 9)]


def test_on_constraint_pkey_do_nothing(storage, session):
    # The primary key's default constraint name is <table>_pkey.
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 9) ON CONFLICT ON CONSTRAINT t_pkey DO NOTHING",
    )
    assert res.command_tag == "INSERT 0 0"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 5)]


def test_on_constraint_pkey_do_update(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 9) "
        "ON CONFLICT ON CONSTRAINT t_pkey DO UPDATE SET n = EXCLUDED.n",
    )
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 9)]


def test_on_constraint_named_unique(storage, session):
    q(
        storage,
        session,
        "CREATE TABLE u (id bigint PRIMARY KEY, email text, CONSTRAINT u_email_uq UNIQUE (email))",
    )
    q(storage, session, "INSERT INTO u (id, email) VALUES (1, 'a@x.com')")
    res = q(
        storage,
        session,
        "INSERT INTO u (id, email) VALUES (2, 'a@x.com') "
        "ON CONFLICT ON CONSTRAINT u_email_uq DO UPDATE SET email = 'b@x.com'",
    )
    assert res.command_tag == "INSERT 0 1"
    # The conflicting row (id=1) was updated; no new row inserted.
    assert q(storage, session, "SELECT id, email FROM u ORDER BY id").rows == [(1, "b@x.com")]


def test_on_constraint_unknown_name_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        q(
            storage,
            session,
            "INSERT INTO t (id, n) VALUES (1, 9) ON CONFLICT ON CONSTRAINT nope DO NOTHING",
        )
    assert ei.value.sqlstate == "42704"


def test_do_update_without_target_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        q(storage, session, "INSERT INTO t (id, n) VALUES (1, 9) ON CONFLICT DO UPDATE SET n = 1")
    assert ei.value.sqlstate == "42601"


def test_multi_row_mixed_insert_and_update(storage, session):
    # id=1 conflicts -> updated; id=2 fresh -> inserted. Tag counts both.
    res = q(
        storage,
        session,
        "INSERT INTO t (id, n) VALUES (1, 50), (2, 60) "
        "ON CONFLICT (id) DO UPDATE SET n = EXCLUDED.n",
    )
    assert res.command_tag == "INSERT 0 2"
    assert q(storage, session, "SELECT id, n FROM t ORDER BY id").rows == [(1, 50), (2, 60)]
