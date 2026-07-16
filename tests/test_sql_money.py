"""money type + to_char numeric formatting (#114)."""

from __future__ import annotations

from decimal import Decimal

import pytest

from secantus.sql import numformat, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure numformat.py
# --------------------------------------------------------------------------- #


def test_parse_money():
    assert numformat.parse_money("1234.5") == Decimal("1234.50")
    assert numformat.parse_money("$1,234.56") == Decimal("1234.56")
    assert numformat.parse_money("(1234.56)") == Decimal("-1234.56")
    with pytest.raises(numformat.MoneyError):
        numformat.parse_money("nope")


def test_render_money():
    assert numformat.render_money(Decimal("1234.5")) == "$1,234.50"
    assert numformat.render_money(Decimal("-1234.567")) == "-$1,234.57"
    assert numformat.render_money(0) == "$0.00"
    assert numformat.render_money(Decimal("1000000")) == "$1,000,000.00"


def test_to_char_fill_mode():
    assert numformat.to_char_numeric(1234.56, "FM999999.99") == "1234.56"
    assert numformat.to_char_numeric(1234.56, "FM9,999,999.99") == "1,234.56"
    assert numformat.to_char_numeric(1234.5, "FM999.00") == "1234.50"
    assert numformat.to_char_numeric(7, "FM0009") == "0007"


def test_to_char_currency():
    assert numformat.to_char_numeric(1234.56, "FM$9,999.99") == "$1,234.56"
    assert numformat.to_char_numeric(1234.56, "FML9999.99") == "$1234.56"


def test_to_char_signs():
    assert numformat.to_char_numeric(5, "FMS999") == "+5"
    assert numformat.to_char_numeric(-5, "FMS999") == "-5"
    assert numformat.to_char_numeric(-1234.5, "FM9999.99MI") == "1234.50-"
    assert numformat.to_char_numeric(-1234.5, "FM9999.99PR") == "<1234.50>"


def test_to_char_padding_non_fm():
    assert numformat.to_char_numeric(1234.56, "999999.99") == "   1234.56"
    assert numformat.to_char_numeric(-5, "FM999") == "-5"


# --------------------------------------------------------------------------- #
# SQL surface
# --------------------------------------------------------------------------- #


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


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


def rendered(storage, session, sql):
    r = run(storage, session, sql)
    from secantus.sql import typemap

    return typemap.to_pg_text(r.rows[0][0], r.columns[0].type_tag).decode()


def test_money_cast_typed(storage, session):
    assert col(storage, session, "SELECT '1234.5'::money").type_tag == "money"


def test_money_cast_renders(storage, session):
    assert rendered(storage, session, "SELECT '1234.5'::money") == "$1,234.50"
    assert rendered(storage, session, "SELECT '$1,234.56'::money") == "$1,234.56"


def test_to_char_numeric_typed_text(storage, session):
    assert col(storage, session, "SELECT to_char(1234.5, 'FM999,999.99')").type_tag == "text"


def test_to_char_numeric_value(storage, session):
    assert val(storage, session, "SELECT to_char(1234.5, 'FM999,999.99')") == "1,234.50"
    assert val(storage, session, "SELECT to_char(1234, 'FM$9,999.99')") == "$1,234.00"
    assert val(storage, session, "SELECT to_char(-1234.5, 'FM9999.99PR')") == "<1234.50>"


def test_to_char_timestamp_still_works(storage, session):
    # The numeric routing must not break the timestamp form.
    assert val(storage, session, "SELECT to_char(timestamp '2020-03-15', 'YYYY-MM-DD')") == (
        "2020-03-15"
    )


@pytest.fixture
def items(storage, session):
    run(storage, session, "CREATE TABLE items (id int PRIMARY KEY, price money)")
    run(storage, session, "INSERT INTO items VALUES (1, '19.99')")
    run(storage, session, "INSERT INTO items VALUES (2, '$1,250.00')")
    return storage


def test_money_column_roundtrip(items, session):
    assert rendered(items, session, "SELECT price FROM items WHERE id = 2") == "$1,250.00"


def test_money_column_typed(items, session):
    assert col(items, session, "SELECT price FROM items WHERE id = 1").type_tag == "money"


def test_money_arithmetic(items, session):
    assert rendered(items, session, "SELECT price + price FROM items WHERE id = 1") == "$39.98"
    assert rendered(items, session, "SELECT price * 2 FROM items WHERE id = 1") == "$39.98"
    assert col(items, session, "SELECT price * 2 FROM items WHERE id = 1").type_tag == "money"


def test_money_where_equality(items, session):
    assert val(items, session, "SELECT id FROM items WHERE price = '19.99'") == 1


def test_money_where_range(items, session):
    ids = [
        r[0]
        for r in run(
            items, session, "SELECT id FROM items WHERE price > '100'::money ORDER BY id"
        ).rows
    ]
    assert ids == [2]
