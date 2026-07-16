"""Regex / string scalar functions: ``regexp_replace`` / ``regexp_matches`` /
``regexp_count`` / ``split_part`` / ``translate`` (evaluated per row in
``secantus.sql.scalar``).
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
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


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, s text)")
    run(storage, session, "INSERT INTO t VALUES (1,'Hello World'),(2,'a1b2c3'),(3,'x/y/z')")
    return storage


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


# -- regexp_replace ----------------------------------------------------------- #


def test_regexp_replace_first_match(t, session):
    assert val(t, session, "SELECT regexp_replace(s, 'o', '0') FROM t WHERE id=1") == "Hell0 World"


def test_regexp_replace_global(t, session):
    assert (
        val(t, session, "SELECT regexp_replace(s, 'o', '0', 'g') FROM t WHERE id=1")
        == "Hell0 W0rld"
    )


def test_regexp_replace_backrefs(t, session):
    assert (
        val(t, session, r"SELECT regexp_replace(s, '(\w)(\d)', '\2\1', 'g') FROM t WHERE id=2")
        == "1a2b3c"
    )


def test_regexp_replace_whole_match_ampersand(t, session):
    assert (
        val(t, session, r"SELECT regexp_replace(s, '\d', '[\&]', 'g') FROM t WHERE id=2")
        == "a[1]b[2]c[3]"
    )


def test_regexp_replace_case_insensitive(t, session):
    assert val(t, session, "SELECT regexp_replace(s, 'hello', 'HI', 'i') FROM t WHERE id=1") == (
        "HI World"
    )


def test_regexp_replace_null(t, session):
    assert val(t, session, "SELECT regexp_replace(NULL, 'a', 'b') FROM t WHERE id=1") is None


# -- split_part --------------------------------------------------------------- #


def test_split_part(t, session):
    assert val(t, session, "SELECT split_part(s, '/', 2) FROM t WHERE id=3") == "y"


def test_split_part_out_of_range(t, session):
    assert val(t, session, "SELECT split_part(s, '/', 9) FROM t WHERE id=3") == ""


def test_split_part_negative_from_end(t, session):
    assert val(t, session, "SELECT split_part(s, '/', -1) FROM t WHERE id=3") == "z"


# -- translate ---------------------------------------------------------------- #


def test_translate(t, session):
    assert val(t, session, "SELECT translate(s, 'lo', 'LO') FROM t WHERE id=1") == "HeLLO WOrLd"


def test_translate_deletes_extra_source_chars(t, session):
    # 'o' has no counterpart in the shorter `to` string, so it is deleted.
    assert val(t, session, "SELECT translate(s, 'lo', 'L') FROM t WHERE id=1") == "HeLL WrLd"


# -- regexp_count ------------------------------------------------------------- #


def test_regexp_count(t, session):
    assert val(t, session, "SELECT regexp_count(s, '[0-9]') FROM t WHERE id=2") == 3


def test_regexp_count_zero(t, session):
    assert val(t, session, "SELECT regexp_count(s, 'zzz') FROM t WHERE id=2") == 0


# -- regexp_matches ----------------------------------------------------------- #


def test_regexp_matches_groups(t, session):
    assert val(t, session, r"SELECT regexp_matches(s, '(\w)(\d)') FROM t WHERE id=2") == ["a", "1"]


def test_regexp_matches_no_match(t, session):
    assert val(t, session, "SELECT regexp_matches(s, 'zzz') FROM t WHERE id=2") is None


def test_regexp_matches_whole_when_no_group(t, session):
    assert val(t, session, r"SELECT regexp_matches(s, '\d+') FROM t WHERE id=2") == ["1"]
