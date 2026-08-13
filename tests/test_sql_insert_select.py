"""``INSERT INTO target [(cols)] SELECT ...`` — insert the rows of a query.

The source query runs first (it may filter / join / aggregate / be a set
operation or a CTE); its result rows map positionally onto the target columns,
coerced to each target column's type, and are inserted like a VALUES batch.
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
    s.q("CREATE TABLE src (id bigint primary key, region text, amount int)")
    for i, r, a in [(1, "east", 10), (2, "east", 20), (3, "west", 30)]:
        s.q(f"INSERT INTO src (id, region, amount) VALUES ({i}, '{r}', {a})")
    s.q("CREATE TABLE dst (id bigint primary key, region text, amount int)")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_insert_select_basic(storage, session):
    res = q(
        storage, session, "INSERT INTO dst (id, region, amount) SELECT id, region, amount FROM src"
    )
    assert res.command_tag == "INSERT 0 3"
    rows = q(storage, session, "SELECT id, region, amount FROM dst ORDER BY id").rows
    assert rows == [(1, "east", 10), (2, "east", 20), (3, "west", 30)]


def test_insert_select_with_filter(storage, session):
    q(
        storage,
        session,
        "INSERT INTO dst (id, region, amount) "
        "SELECT id, region, amount FROM src WHERE amount >= 20",
    )
    rows = q(storage, session, "SELECT id FROM dst ORDER BY id").rows
    assert rows == [(2,), (3,)]


def test_insert_select_no_column_list(storage, session):
    res = q(storage, session, "INSERT INTO dst SELECT id, region, amount FROM src WHERE id = 1")
    assert res.command_tag == "INSERT 0 1"
    assert q(storage, session, "SELECT region FROM dst").rows == [("east",)]


def test_insert_select_column_subset(storage, session):
    # Only id + region provided; amount is nullable so it's allowed to be absent.
    q(storage, session, "INSERT INTO dst (id, region) SELECT id, region FROM src WHERE id = 2")
    row = q(storage, session, "SELECT id, region, amount FROM dst").rows[0]
    assert row == (2, "east", None)


def test_insert_select_from_aggregate(storage, session):
    # Source is a GROUP BY; the target is keyed by region (a text PK).
    storage.q("CREATE TABLE by_region (region text primary key, total int)")
    q(
        storage,
        session,
        "INSERT INTO by_region (region, total) SELECT region, SUM(amount) FROM src GROUP BY region",
    )
    rows = q(storage, session, "SELECT region, total FROM by_region ORDER BY region").rows
    assert rows == [("east", 30), ("west", 30)]


def test_insert_select_from_set_operation(storage, session):
    storage.q("CREATE TABLE amounts (v int primary key)")
    q(
        storage,
        session,
        "INSERT INTO amounts (v) SELECT amount FROM src WHERE region = 'east' "
        "UNION SELECT amount FROM src WHERE region = 'west'",
    )
    rows = q(storage, session, "SELECT v FROM amounts ORDER BY v").rows
    assert rows == [(10,), (20,), (30,)]


def test_insert_select_type_coercion(storage, session):
    # Source amount is int; target price is numeric — coerced on insert.
    storage.q("CREATE TABLE priced (id bigint primary key, price numeric)")
    q(storage, session, "INSERT INTO priced (id, price) SELECT id, amount FROM src WHERE id = 1")
    from decimal import Decimal

    assert q(storage, session, "SELECT price FROM priced").rows == [(Decimal("10"),)]


def test_insert_select_returning(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO dst (id, region, amount) SELECT id, region, amount FROM src "
        "WHERE id = 3 RETURNING id, region",
    )
    assert [c.name for c in res.columns] == ["id", "region"]
    assert res.rows == [(3, "west")]


def test_insert_select_column_count_mismatch(storage, session):
    with pytest.raises(SQLError) as ei:
        q(storage, session, "INSERT INTO dst (id, region, amount) SELECT id, region FROM src")
    assert ei.value.sqlstate == "42601"


def test_insert_select_not_null_violation(storage, session):
    # Target id (PK) is NOT NULL; selecting a NULL into it is rejected.
    storage.q("CREATE TABLE need_id (id bigint primary key, v int)")
    with pytest.raises(SQLError) as ei:
        q(storage, session, "INSERT INTO need_id (id, v) SELECT NULL, amount FROM src WHERE id = 1")
    assert ei.value.sqlstate == "23502"


def test_insert_select_empty_source(storage, session):
    res = q(
        storage,
        session,
        "INSERT INTO dst (id, region, amount) "
        "SELECT id, region, amount FROM src WHERE amount > 1000",
    )
    assert res.command_tag == "INSERT 0 0"
    assert q(storage, session, "SELECT count(*) FROM dst").rows == [(0,)]


# --------------------------------------------------------------------------- #
# Prefix VALUES rows (no explicit column list) — Postgres fills the rest
# --------------------------------------------------------------------------- #


def test_short_values_row_fills_column_prefix(storage, session):
    q(
        storage,
        session,
        "CREATE TABLE pv (a int, b int, c text DEFAULT 'dflt')",
    )
    q(storage, session, "INSERT INTO pv VALUES (1, 2)")
    q(storage, session, "INSERT INTO pv VALUES ((3), (4)), ((5), (6))")
    assert q(storage, session, "SELECT * FROM pv ORDER BY a").rows == [
        (1, 2, "dflt"),
        (3, 4, "dflt"),
        (5, 6, "dflt"),
    ]


def test_too_many_values_rejected(storage, session):
    q(storage, session, "CREATE TABLE pv2 (a int, b int)")
    with pytest.raises(SQLError) as e:
        q(storage, session, "INSERT INTO pv2 VALUES (1, 2, 3)")
    assert "more expressions than target columns" in str(e.value)


def test_explicit_column_list_still_requires_exact_arity(storage, session):
    q(storage, session, "CREATE TABLE pv3 (a int, b int)")
    with pytest.raises(SQLError) as e:
        q(storage, session, "INSERT INTO pv3 (a, b) VALUES (1)")
    assert "more target columns than expressions" in str(e.value)
