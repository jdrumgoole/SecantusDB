"""``ALTER TABLE`` — ADD / DROP / RENAME COLUMN, RENAME TO, SET/DROP NOT NULL.

The catalog is the source of SQL truth, so each action rewrites the table's
catalog doc; where the *data* has to follow (a dropped column's field is
``$unset``, a renamed non-PK column's field is ``$rename``d) the backing
collection is updated too. The PK column maps to ``_id``, so renaming it only
changes the SQL name, never the stored field.
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
    run_sql(s, DB, "CREATE TABLE t (id int primary key, a text, b int)", session=session)
    run_sql(s, DB, "INSERT INTO t (id, a, b) VALUES (1, 'x', 10), (2, 'y', 20)", session=session)
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return q(storage, session, sql).rows


# -- ADD COLUMN ------------------------------------------------------------- #


def test_add_column(storage, session):
    res = q(storage, session, "ALTER TABLE t ADD COLUMN c text")
    assert res.command_tag == "ALTER TABLE"
    assert rows(storage, session, "SELECT id, a, b, c FROM t ORDER BY id") == [
        (1, "x", 10, None),
        (2, "y", 20, None),
    ]


def test_add_column_then_select_and_update(storage, session):
    q(storage, session, "ALTER TABLE t ADD COLUMN c int")
    run_sql(storage, DB, "UPDATE t SET c = 99 WHERE id = 1", session=session)
    assert rows(storage, session, "SELECT id, c FROM t ORDER BY id") == [(1, 99), (2, None)]


def test_add_column_duplicate_errors(storage, session):
    with pytest.raises(errors.SQLError) as ei:
        q(storage, session, "ALTER TABLE t ADD COLUMN a text")
    assert ei.value.sqlstate == "42701"


def test_add_column_if_not_exists_is_noop(storage, session):
    res = q(storage, session, "ALTER TABLE t ADD COLUMN IF NOT EXISTS a text")
    assert res.command_tag == "ALTER TABLE"
    # 'a' keeps its original type/values.
    assert rows(storage, session, "SELECT a FROM t ORDER BY id") == [("x",), ("y",)]


def test_add_column_not_null_marks_nullable_false(storage, session):
    q(storage, session, "ALTER TABLE t ADD COLUMN d int NOT NULL")
    res = q(
        storage,
        session,
        "SELECT is_nullable FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'd'",
    )
    assert res.rows == [("NO",)]


# -- DROP COLUMN ------------------------------------------------------------ #


def test_drop_column_removes_field(storage, session):
    q(storage, session, "ALTER TABLE t DROP COLUMN b")
    assert rows(storage, session, "SELECT id, a FROM t ORDER BY id") == [(1, "x"), (2, "y")]
    with pytest.raises(errors.SQLError):
        q(storage, session, "SELECT b FROM t")
    # The underlying field is gone, not just hidden.
    assert all("b" not in d for d in storage.find_matching(DB, "t"))


def test_drop_column_unknown_errors(storage, session):
    with pytest.raises(errors.SQLError):
        q(storage, session, "ALTER TABLE t DROP COLUMN nope")


def test_drop_column_if_exists_is_noop(storage, session):
    res = q(storage, session, "ALTER TABLE t DROP COLUMN IF EXISTS nope")
    assert res.command_tag == "ALTER TABLE"


def test_drop_primary_key_column_rejected(storage, session):
    with pytest.raises(errors.SQLError):
        q(storage, session, "ALTER TABLE t DROP COLUMN id")


# -- RENAME COLUMN ---------------------------------------------------------- #


def test_rename_column_renames_field(storage, session):
    q(storage, session, "ALTER TABLE t RENAME COLUMN a TO aa")
    assert rows(storage, session, "SELECT id, aa, b FROM t ORDER BY id") == [
        (1, "x", 10),
        (2, "y", 20),
    ]
    with pytest.raises(errors.SQLError):
        q(storage, session, "SELECT a FROM t")


def test_rename_pk_column_keeps_id_field(storage, session):
    q(storage, session, "ALTER TABLE t RENAME COLUMN id TO pk")
    assert rows(storage, session, "SELECT pk, a FROM t ORDER BY pk") == [(1, "x"), (2, "y")]
    # The PK is still stored under _id.
    assert all("_id" in d for d in storage.find_matching(DB, "t"))


def test_rename_column_conflict_errors(storage, session):
    with pytest.raises(errors.SQLError) as ei:
        q(storage, session, "ALTER TABLE t RENAME COLUMN a TO b")
    assert ei.value.sqlstate == "42701"


def test_rename_column_unknown_errors(storage, session):
    with pytest.raises(errors.SQLError):
        q(storage, session, "ALTER TABLE t RENAME COLUMN nope TO x")


# -- RENAME TO -------------------------------------------------------------- #


def test_rename_table(storage, session):
    q(storage, session, "ALTER TABLE t RENAME TO t2")
    assert rows(storage, session, "SELECT id, a FROM t2 ORDER BY id") == [(1, "x"), (2, "y")]
    with pytest.raises(errors.SQLError):
        q(storage, session, "SELECT id FROM t")


# -- ALTER COLUMN SET / DROP NOT NULL --------------------------------------- #


def test_set_and_drop_not_null(storage, session):
    q(storage, session, "ALTER TABLE t ALTER COLUMN a SET NOT NULL")
    res = q(
        storage,
        session,
        "SELECT is_nullable FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'a'",
    )
    assert res.rows == [("NO",)]
    q(storage, session, "ALTER TABLE t ALTER COLUMN a DROP NOT NULL")
    res = q(
        storage,
        session,
        "SELECT is_nullable FROM information_schema.columns "
        "WHERE table_name = 't' AND column_name = 'a'",
    )
    assert res.rows == [("YES",)]


# -- table-level guards ----------------------------------------------------- #


def test_alter_missing_table_errors(storage, session):
    with pytest.raises(errors.SQLError):
        q(storage, session, "ALTER TABLE nope ADD COLUMN x int")


def test_alter_if_exists_missing_table_is_noop(storage, session):
    res = q(storage, session, "ALTER TABLE IF EXISTS nope ADD COLUMN x int")
    assert res.command_tag == "ALTER TABLE"


# -- multi-action (mixed-kind) ALTER TABLE (#145) --------------------------- #


def test_multi_action_mixed_add_drop(storage, session):
    q(storage, session, "ALTER TABLE t ADD COLUMN c int, DROP COLUMN b")
    names = [col.name for col in q(storage, session, "SELECT * FROM t").columns]
    assert names == ["id", "a", "c"]
    # existing rows keep their data; the new column reads NULL.
    assert rows(storage, session, "SELECT id, a, c FROM t ORDER BY id") == [
        (1, "x", None),
        (2, "y", None),
    ]


def test_multi_action_rename_add_drop(storage, session):
    q(storage, session, "ALTER TABLE t RENAME COLUMN a TO aa, ADD COLUMN c text, DROP COLUMN b")
    names = [col.name for col in q(storage, session, "SELECT * FROM t").columns]
    assert names == ["id", "aa", "c"]
    # the renamed column keeps its values.
    assert rows(storage, session, "SELECT id, aa FROM t ORDER BY id") == [(1, "x"), (2, "y")]


def test_multi_action_add_column_and_constraint(storage, session):
    q(storage, session, "ALTER TABLE t ADD COLUMN c int, ADD CONSTRAINT ck CHECK (c >= 0)")
    names = [col.name for col in q(storage, session, "SELECT * FROM t").columns]
    assert names == ["id", "a", "b", "c"]
    # the CHECK is enforced on write.
    with pytest.raises(errors.SQLError):
        q(storage, session, "INSERT INTO t (id, c) VALUES (3, -1)")


def test_multi_action_if_exists(storage, session):
    q(storage, session, "ALTER TABLE IF EXISTS t ADD COLUMN c int, DROP COLUMN a")
    names = [col.name for col in q(storage, session, "SELECT * FROM t").columns]
    assert names == ["id", "b", "c"]


def test_multi_action_if_exists_missing_table_is_noop(storage, session):
    # IF EXISTS on a missing table is a silent no-op even with multiple actions.
    q(storage, session, "ALTER TABLE IF EXISTS nope ADD COLUMN c int, DROP COLUMN d")
