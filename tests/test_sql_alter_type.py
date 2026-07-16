"""``ALTER TYPE … ADD VALUE`` — extend an enum with a new label (optionally
positioned BEFORE / AFTER an existing one), and enum-aware ``ORDER BY`` that sorts
by the enum's declared label order rather than lexically.

Enum values are stored as their label text, so a naive ORDER BY would sort them
alphabetically; the planner records the declared label list and the executor maps
each value to its ordinal so the sort follows the type's declared order.
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


def labels(storage):
    return storage.find_matching(DB, "__sql_enums__", {"_id": "mood"})[0]["labels"]


@pytest.fixture
def mood(storage, session):
    run(storage, session, "CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')")
    return storage


# -- ADD VALUE ---------------------------------------------------------------- #


def test_add_value_appends(mood, session):
    assert run(mood, session, "ALTER TYPE mood ADD VALUE 'ecstatic'").command_tag == "ALTER TYPE"
    assert labels(mood) == ["sad", "ok", "happy", "ecstatic"]


def test_add_value_after(mood, session):
    run(mood, session, "ALTER TYPE mood ADD VALUE 'meh' AFTER 'ok'")
    assert labels(mood) == ["sad", "ok", "meh", "happy"]


def test_add_value_before(mood, session):
    run(mood, session, "ALTER TYPE mood ADD VALUE 'awful' BEFORE 'sad'")
    assert labels(mood) == ["awful", "sad", "ok", "happy"]


def test_add_value_duplicate_rejected(mood, session):
    assert sqlstate(mood, session, "ALTER TYPE mood ADD VALUE 'ok'") == "42710"


def test_add_value_if_not_exists_is_noop(mood, session):
    assert (
        run(mood, session, "ALTER TYPE mood ADD VALUE IF NOT EXISTS 'ok'").command_tag
        == "ALTER TYPE"
    )
    assert labels(mood) == ["sad", "ok", "happy"]


def test_add_value_unknown_type_rejected(mood, session):
    assert sqlstate(mood, session, "ALTER TYPE nope ADD VALUE 'x'") == "42704"


def test_add_value_unknown_neighbour_rejected(mood, session):
    assert sqlstate(mood, session, "ALTER TYPE mood ADD VALUE 'x' AFTER 'zzz'") == "42704"


def test_rename_value_unsupported(mood, session):
    assert sqlstate(mood, session, "ALTER TYPE mood RENAME VALUE 'ok' TO 'fine'") == "0A000"


def test_added_label_valid_on_write(mood, session):
    run(mood, session, "ALTER TYPE mood ADD VALUE 'great'")
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t VALUES (1, 'great')")
    assert run(mood, session, "SELECT m FROM t").rows == [("great",)]
    assert sqlstate(mood, session, "INSERT INTO t VALUES (2, 'bogus')") == "22P02"


def test_label_with_apostrophe(mood, session):
    run(mood, session, "ALTER TYPE mood ADD VALUE 'o''brien'")
    assert labels(mood)[-1] == "o'brien"


# -- enum-aware ORDER BY ------------------------------------------------------ #


@pytest.fixture
def moods_table(mood, session):
    run(mood, session, "CREATE TABLE t (id int PRIMARY KEY, m mood)")
    run(mood, session, "INSERT INTO t VALUES (1, 'happy'), (2, 'sad'), (3, 'ok')")
    return mood


def test_order_by_enum_is_declared_order_not_lexical(moods_table, session):
    # Lexical order would be happy, ok, sad; declared order is sad, ok, happy.
    rows = run(moods_table, session, "SELECT id, m FROM t ORDER BY m").rows
    assert rows == [(2, "sad"), (3, "ok"), (1, "happy")]


def test_order_by_enum_desc(moods_table, session):
    rows = run(moods_table, session, "SELECT m FROM t ORDER BY m DESC").rows
    assert rows == [("happy",), ("ok",), ("sad",)]


def test_order_by_enum_after_add_value_positions_correctly(moods_table, session):
    run(moods_table, session, "ALTER TYPE mood ADD VALUE 'meh' AFTER 'ok'")
    run(moods_table, session, "INSERT INTO t VALUES (4, 'meh')")
    rows = run(moods_table, session, "SELECT m FROM t ORDER BY m").rows
    # meh sorts between ok and happy — its declared position, not alphabetical.
    assert rows == [("sad",), ("ok",), ("meh",), ("happy",)]


def test_order_by_enum_nulls_last_default(moods_table, session):
    run(moods_table, session, "INSERT INTO t VALUES (9, NULL)")
    rows = run(moods_table, session, "SELECT id FROM t ORDER BY m").rows
    assert rows[-1] == (9,)  # NULL sorts last (Postgres ASC default)
