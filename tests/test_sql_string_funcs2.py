"""String round-out scalar functions: ``lpad`` / ``rpad`` / ``left`` / ``right`` /
``repeat`` / ``reverse`` / ``initcap`` / ``ascii`` / ``chr`` / ``position`` /
``strpos`` / ``overlay`` (evaluated per row in ``secantus.sql.scalar``).
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
    run(storage, session, "INSERT INTO t VALUES (1, 'hello')")
    return storage


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


# -- lpad / rpad -------------------------------------------------------------- #


def test_lpad_default_space(t, session):
    assert val(t, session, "SELECT lpad(s, 8) FROM t") == "   hello"


def test_lpad_fill(t, session):
    assert val(t, session, "SELECT lpad(s, 8, '*') FROM t") == "***hello"


def test_rpad_fill(t, session):
    assert val(t, session, "SELECT rpad(s, 8, '*') FROM t") == "hello***"


def test_lpad_truncates(t, session):
    assert val(t, session, "SELECT lpad(s, 3) FROM t") == "hel"


# -- left / right ------------------------------------------------------------- #


def test_left(t, session):
    assert val(t, session, "SELECT left(s, 3) FROM t") == "hel"


def test_left_negative(t, session):
    assert val(t, session, "SELECT left(s, -2) FROM t") == "hel"  # drop last 2


def test_right(t, session):
    assert val(t, session, "SELECT right(s, 3) FROM t") == "llo"


def test_right_negative(t, session):
    assert val(t, session, "SELECT right(s, -2) FROM t") == "llo"  # drop first 2


def test_right_zero(t, session):
    assert val(t, session, "SELECT right(s, 0) FROM t") == ""


# -- repeat / reverse / initcap ----------------------------------------------- #


def test_repeat(t, session):
    assert val(t, session, "SELECT repeat(s, 3) FROM t") == "hellohellohello"


def test_reverse(t, session):
    assert val(t, session, "SELECT reverse(s) FROM t") == "olleh"


def test_initcap(t, session):
    assert val(t, session, "SELECT initcap('hello world') FROM t") == "Hello World"


# -- ascii / chr -------------------------------------------------------------- #


def test_ascii(t, session):
    assert val(t, session, "SELECT ascii(s) FROM t") == 104  # 'h'


def test_ascii_types_int(t, session):
    assert col(t, session, "SELECT ascii(s) FROM t").type_tag == "int4"


def test_chr(t, session):
    assert val(t, session, "SELECT chr(65) FROM t") == "A"


# -- position / strpos / overlay ---------------------------------------------- #


def test_position(t, session):
    assert val(t, session, "SELECT position('l' IN s) FROM t") == 3


def test_strpos_absent(t, session):
    assert val(t, session, "SELECT strpos(s, 'z') FROM t") == 0


def test_position_types_int(t, session):
    assert col(t, session, "SELECT position('l' IN s) FROM t").type_tag == "int4"


def test_overlay_for(t, session):
    assert val(t, session, "SELECT overlay(s placing 'XY' from 2 for 3) FROM t") == "hXYo"


def test_overlay_default_span(t, session):
    assert val(t, session, "SELECT overlay(s placing 'XY' from 2) FROM t") == "hXYlo"


# -- typing + NULL propagation ------------------------------------------------ #


def test_text_funcs_type_text(t, session):
    cols = run(t, session, "SELECT lpad(s, 4), reverse(s), chr(65), left(s, 2) FROM t").columns
    assert [c.type_tag for c in cols] == ["text", "text", "text", "text"]


def test_null_propagation(storage, session):
    run(storage, session, "CREATE TABLE n (id int PRIMARY KEY, s text)")
    run(storage, session, "INSERT INTO n VALUES (1, NULL)")
    assert val(storage, session, "SELECT lpad(s, 5) FROM n") is None
    assert val(storage, session, "SELECT reverse(s) FROM n") is None
    assert val(storage, session, "SELECT ascii(s) FROM n") is None
