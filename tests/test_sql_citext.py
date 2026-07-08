"""citext case-insensitive text (#118): case-folding equality / inequality /
range / IN / LIKE comparisons and case-insensitive ORDER BY, with the original
case preserved for storage / display.
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


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


@pytest.fixture
def users(storage, session):
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, name citext)")
    run(storage, session, "INSERT INTO u VALUES (1, 'Alice')")
    run(storage, session, "INSERT INTO u VALUES (2, 'BOB')")
    run(storage, session, "INSERT INTO u VALUES (3, 'carol')")
    return storage


def ids(result):
    return sorted(r[0] for r in result.rows)


def test_cast_typed(storage, session):
    assert col(storage, session, "SELECT 'Foo'::citext").type_tag == "citext"


def test_column_typed(users, session):
    assert col(users, session, "SELECT name FROM u WHERE id = 1").type_tag == "citext"


def test_stores_original_case(users, session):
    # The stored / displayed value keeps its case; only comparisons fold.
    assert val(users, session, "SELECT name FROM u WHERE id = 1") == "Alice"


def test_equality_case_insensitive(users, session):
    assert ids(run(users, session, "SELECT id FROM u WHERE name = 'alice'")) == [1]
    assert ids(run(users, session, "SELECT id FROM u WHERE name = 'BOB'")) == [2]
    assert ids(run(users, session, "SELECT id FROM u WHERE name = 'CAROL'")) == [3]


def test_inequality_case_insensitive(users, session):
    assert ids(run(users, session, "SELECT id FROM u WHERE name != 'ALICE'")) == [2, 3]


def test_in_case_insensitive(users, session):
    assert ids(run(users, session, "SELECT id FROM u WHERE name IN ('ALICE', 'bob')")) == [1, 2]


def test_range_case_insensitive(users, session):
    # 'alice' < 'c' and 'bob' < 'c', but 'carol' is not — folding makes B sort with b.
    assert ids(run(users, session, "SELECT id FROM u WHERE name < 'c'")) == [1, 2]


def test_between_case_insensitive(users, session):
    assert ids(run(users, session, "SELECT id FROM u WHERE name BETWEEN 'a' AND 'bz'")) == [1, 2]


def test_like_is_case_insensitive(users, session):
    # citext LIKE folds case (equivalent to ILIKE).
    assert ids(run(users, session, "SELECT id FROM u WHERE name LIKE 'a%'")) == [1]
    assert ids(run(users, session, "SELECT id FROM u WHERE name LIKE 'B%'")) == [2]


def test_order_by_case_insensitive(users, session):
    # Case-sensitive order would put 'BOB' (B=0x42) before 'Alice' (A=0x41)... no —
    # before 'carol'/'alice'; the point is folding gives alphabetical a,b,c order.
    rows = run(users, session, "SELECT id FROM u ORDER BY name").rows
    assert [r[0] for r in rows] == [1, 2, 3]  # Alice, BOB, carol


def test_order_by_desc_case_insensitive(users, session):
    rows = run(users, session, "SELECT id FROM u ORDER BY name DESC").rows
    assert [r[0] for r in rows] == [3, 2, 1]  # carol, BOB, Alice
