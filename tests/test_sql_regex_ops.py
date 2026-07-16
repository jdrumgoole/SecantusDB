"""POSIX regex-match operators: ``~`` / ``~*`` / ``!~`` / ``!~*``.

``~`` matches a raw regex **unanchored** (``re.search`` semantics, like Postgres),
``~*`` is case-insensitive, and ``!~`` / ``!~*`` are their negations. They work in
WHERE (lowered to a Mongo ``$regex`` filter), in a SELECT-list boolean expression
(evaluated per row), and in CHECK constraints (table + domain) — closing the regex
gap those CHECKs previously had.
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


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, name text)")
    run(storage, session, "INSERT INTO t VALUES (1,'Alice'),(2,'bob'),(3,'Carol'),(4,'alfred')")
    return storage


def names(storage, session, sql):
    return sorted(r[0] for r in run(storage, session, sql).rows)


# -- WHERE filters ------------------------------------------------------------ #


def test_match_anchored(t, session):
    assert names(t, session, "SELECT name FROM t WHERE name ~ '^A'") == ["Alice"]


def test_match_unanchored_is_search(t, session):
    # ~ is a partial (search) match, not anchored like LIKE.
    assert names(t, session, "SELECT name FROM t WHERE name ~ 'o'") == ["Carol", "bob"]


def test_case_insensitive_match(t, session):
    assert names(t, session, "SELECT name FROM t WHERE name ~* '^a'") == ["Alice", "alfred"]


def test_negated_match(t, session):
    assert names(t, session, "SELECT name FROM t WHERE name !~ '^A'") == ["Carol", "alfred", "bob"]


def test_negated_case_insensitive_match(t, session):
    assert names(t, session, "SELECT name FROM t WHERE name !~* '^a'") == ["Carol", "bob"]


def test_char_class_pattern(t, session):
    assert names(t, session, "SELECT name FROM t WHERE name ~ '[A-Z]'") == ["Alice", "Carol"]


def test_combined_with_and(t, session):
    rows = names(t, session, "SELECT name FROM t WHERE name ~* 'a' AND name !~ '^A'")
    assert rows == ["Carol", "alfred"]


# -- SELECT-list boolean ------------------------------------------------------ #


def test_regex_in_select_list(t, session):
    rows = run(t, session, "SELECT name, (name ~ '^A') AS m FROM t ORDER BY id").rows
    assert rows == [("Alice", True), ("bob", False), ("Carol", False), ("alfred", False)]


def test_regex_in_select_list_case_insensitive(t, session):
    rows = run(t, session, "SELECT (name ~* '^a') AS m FROM t ORDER BY id").rows
    assert [r[0] for r in rows] == [True, False, False, True]


# -- CHECK constraints (the gap this slice closes) ---------------------------- #


def test_table_check_with_regex(storage, session):
    run(
        storage, session, "CREATE TABLE b (id int PRIMARY KEY, code text CHECK (code ~ '^[A-Z]+$'))"
    )
    run(storage, session, "INSERT INTO b VALUES (1, 'ABC')")  # OK
    assert sqlstate(storage, session, "INSERT INTO b VALUES (2, 'a1')") == "23514"


def test_domain_check_with_regex(storage, session):
    run(storage, session, "CREATE DOMAIN zip AS text CHECK (VALUE ~ '^[0-9]{5}$')")
    run(storage, session, "CREATE TABLE a (id int PRIMARY KEY, z zip)")
    run(storage, session, "INSERT INTO a VALUES (1, '12345')")  # OK
    assert run(storage, session, "SELECT z FROM a").rows == [("12345",)]
    assert sqlstate(storage, session, "INSERT INTO a VALUES (2, 'abc')") == "23514"


def test_domain_check_case_insensitive_regex(storage, session):
    run(storage, session, "CREATE DOMAIN hex AS text CHECK (VALUE ~* '^[0-9a-f]+$')")
    run(storage, session, "CREATE TABLE h (id int PRIMARY KEY, v hex)")
    run(storage, session, "INSERT INTO h VALUES (1, 'DEADbeef')")  # OK (case-insensitive)
    assert sqlstate(storage, session, "INSERT INTO h VALUES (2, 'xyz')") == "23514"


# -- NULL handling ------------------------------------------------------------ #


def test_null_not_matched_by_positive_regex(storage, session):
    run(storage, session, "CREATE TABLE n (id int PRIMARY KEY, name text)")
    run(storage, session, "INSERT INTO n VALUES (1, NULL), (2, 'x')")
    # A NULL value never satisfies a positive ~ match. (Negation ~ !~ inherits the
    # layer's existing NULL-in-negation divergence, shared with != / NOT LIKE — a
    # NULL row leaks into the negated result; not specific to regex.)
    assert names(storage, session, "SELECT name FROM n WHERE name ~ 'x'") == ["x"]
