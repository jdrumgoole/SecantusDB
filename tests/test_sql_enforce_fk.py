"""Foreign-key enforcement on write (``23503``).

Child side: an INSERT/UPDATE whose FK columns are all non-NULL requires a
matching parent row. Parent side: DELETE/UPDATE of a referenced row applies the
declared referential action — NO ACTION / RESTRICT reject, CASCADE propagates,
SET NULL / SET DEFAULT clears the child columns. MATCH SIMPLE: a NULL in any FK
column exempts the child row.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))
    run_sql(s, DB, "CREATE TABLE users (id bigint primary key, name text)", session=session)
    run_sql(s, DB, "INSERT INTO users (id, name) VALUES (1, 'a'), (2, 'b')", session=session)
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


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def _child(storage, session, on_delete="", on_update=""):
    clause = f"REFERENCES users(id) {on_delete} {on_update}".strip()
    run(storage, session, f"CREATE TABLE orders (id bigint primary key, uid bigint {clause})")


def test_insert_missing_parent_rejected(storage, session):
    _child(storage, session)
    assert sqlstate(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 99)") == "23503"


def test_insert_valid_parent_ok(storage, session):
    _child(storage, session)
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1)")
    assert rows(storage, session, "SELECT count(*) FROM orders") == [(1,)]


def test_insert_null_fk_exempt(storage, session):
    _child(storage, session)
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, NULL)")
    assert rows(storage, session, "SELECT count(*) FROM orders") == [(1,)]


def test_update_child_to_missing_parent_rejected(storage, session):
    _child(storage, session)
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1)")
    assert sqlstate(storage, session, "UPDATE orders SET uid = 77 WHERE id = 10") == "23503"
    assert rows(storage, session, "SELECT uid FROM orders WHERE id = 10") == [(1,)]


def test_update_child_to_valid_parent_ok(storage, session):
    _child(storage, session)
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1)")
    run(storage, session, "UPDATE orders SET uid = 2 WHERE id = 10")
    assert rows(storage, session, "SELECT uid FROM orders WHERE id = 10") == [(2,)]


def test_delete_parent_no_action_rejected(storage, session):
    _child(storage, session)  # default NO ACTION
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1)")
    assert sqlstate(storage, session, "DELETE FROM users WHERE id = 1") == "23503"
    assert rows(storage, session, "SELECT count(*) FROM users WHERE id = 1") == [(1,)]


def test_delete_parent_with_no_children_ok(storage, session):
    _child(storage, session)
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1)")
    # user 2 has no orders → deletable.
    run(storage, session, "DELETE FROM users WHERE id = 2")
    assert rows(storage, session, "SELECT count(*) FROM users") == [(1,)]


def test_delete_parent_cascade(storage, session):
    _child(storage, session, on_delete="ON DELETE CASCADE")
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1), (11, 1), (12, 2)")
    run(storage, session, "DELETE FROM users WHERE id = 1")
    assert rows(storage, session, "SELECT count(*) FROM orders") == [(1,)]
    assert rows(storage, session, "SELECT uid FROM orders") == [(2,)]


def test_delete_parent_set_null(storage, session):
    _child(storage, session, on_delete="ON DELETE SET NULL")
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1)")
    run(storage, session, "DELETE FROM users WHERE id = 1")
    assert rows(storage, session, "SELECT uid FROM orders WHERE id = 10") == [(None,)]
    assert rows(storage, session, "SELECT count(*) FROM users WHERE id = 1") == [(0,)]


def test_multi_level_cascade(storage, session):
    # users <- orders <- items, both ON DELETE CASCADE.
    _child(storage, session, on_delete="ON DELETE CASCADE")
    run(
        storage,
        session,
        "CREATE TABLE items (id bigint primary key, oid bigint "
        "REFERENCES orders(id) ON DELETE CASCADE)",
    )
    run(storage, session, "INSERT INTO orders (id, uid) VALUES (10, 1)")
    run(storage, session, "INSERT INTO items (id, oid) VALUES (100, 10)")
    run(storage, session, "DELETE FROM users WHERE id = 1")
    assert rows(storage, session, "SELECT count(*) FROM orders") == [(0,)]
    assert rows(storage, session, "SELECT count(*) FROM items") == [(0,)]


def test_table_level_fk_enforced(storage, session):
    run(
        storage,
        session,
        "CREATE TABLE o2 (id bigint primary key, uid bigint, "
        "FOREIGN KEY (uid) REFERENCES users(id))",
    )
    assert sqlstate(storage, session, "INSERT INTO o2 (id, uid) VALUES (1, 42)") == "23503"
    run(storage, session, "INSERT INTO o2 (id, uid) VALUES (1, 1)")
    assert rows(storage, session, "SELECT count(*) FROM o2") == [(1,)]


def test_alter_added_fk_enforced(storage, session):
    run(storage, session, "CREATE TABLE o3 (id bigint primary key, uid bigint)")
    run(storage, session, "ALTER TABLE o3 ADD FOREIGN KEY (uid) REFERENCES users(id)")
    assert sqlstate(storage, session, "INSERT INTO o3 (id, uid) VALUES (1, 42)") == "23503"


def test_references_without_column_list_targets_pk(storage, session):
    run(storage, session, "CREATE TABLE o4 (id bigint primary key, uid bigint REFERENCES users)")
    assert sqlstate(storage, session, "INSERT INTO o4 (id, uid) VALUES (1, 42)") == "23503"
    run(storage, session, "INSERT INTO o4 (id, uid) VALUES (1, 1)")
    assert rows(storage, session, "SELECT count(*) FROM o4") == [(1,)]


def test_insert_select_enforces_fk(storage, session):
    _child(storage, session)
    run(storage, session, "CREATE TABLE src (id bigint primary key, uid bigint)")
    run(storage, session, "INSERT INTO src (id, uid) VALUES (1, 1), (2, 99)")
    assert (
        sqlstate(storage, session, "INSERT INTO orders (id, uid) SELECT id, uid FROM src")
        == "23503"
    )
