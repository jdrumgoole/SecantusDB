"""In-call ``ORDER BY`` for ``array_agg`` / ``string_agg``.

``array_agg(x ORDER BY y)`` and ``string_agg(x, sep ORDER BY y)`` order the
aggregated values by the in-call ORDER BY (multiple keys, ASC/DESC, and Postgres
NULL placement). The value + sort-key pair is pushed per row and sorted in Python.
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
def emp(storage, session):
    run(storage, session, "CREATE TABLE emp (id int PRIMARY KEY, dept text, name text, hired int)")
    run(
        storage,
        session,
        "INSERT INTO emp VALUES (1,'a','carol',30),(2,'a','alice',10),(3,'a','bob',20),"
        "(4,'b','zed',5),(5,'b','amy',9)",
    )
    return storage


# -- array_agg ---------------------------------------------------------------- #


def test_array_agg_order_by_other_column(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, array_agg(name ORDER BY hired) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", ["alice", "bob", "carol"]), ("b", ["zed", "amy"])]


def test_array_agg_order_by_self(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, array_agg(name ORDER BY name) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", ["alice", "bob", "carol"]), ("b", ["amy", "zed"])]


def test_array_agg_order_by_desc(emp, session):
    assert run(emp, session, "SELECT array_agg(name ORDER BY hired DESC) FROM emp").rows == [
        (["carol", "bob", "alice", "amy", "zed"],)
    ]


def test_array_agg_multi_key_order(storage, session):
    run(storage, session, "CREATE TABLE m (id int PRIMARY KEY, a int, b int, v text)")
    run(storage, session, "INSERT INTO m VALUES (1,1,2,'p'),(2,1,1,'q'),(3,2,1,'r')")
    # (a,b): (1,1)->q, (1,2)->p, (2,1)->r
    assert run(storage, session, "SELECT array_agg(v ORDER BY a, b) FROM m").rows == [
        (["q", "p", "r"],)
    ]


def test_array_agg_mixed_directions(storage, session):
    run(storage, session, "CREATE TABLE m (id int PRIMARY KEY, a int, b int, v text)")
    run(storage, session, "INSERT INTO m VALUES (1,1,1,'p'),(2,1,2,'q'),(3,2,1,'r')")
    # a ASC, b DESC: (1,2)->q, (1,1)->p, (2,1)->r
    assert run(storage, session, "SELECT array_agg(v ORDER BY a ASC, b DESC) FROM m").rows == [
        (["q", "p", "r"],)
    ]


# -- string_agg --------------------------------------------------------------- #


def test_string_agg_order_by(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, string_agg(name, ',' ORDER BY name) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", "alice,bob,carol"), ("b", "amy,zed")]


def test_string_agg_order_by_desc(emp, session):
    assert run(emp, session, "SELECT string_agg(name, '|' ORDER BY hired DESC) FROM emp").rows == [
        ("carol|bob|alice|amy|zed",)
    ]


# -- NULL handling ------------------------------------------------------------ #


def test_order_by_nulls_last_on_asc(storage, session):
    run(storage, session, "CREATE TABLE n (id int PRIMARY KEY, k int, v text)")
    run(storage, session, "INSERT INTO n VALUES (1,2,'x'),(2,NULL,'y'),(3,1,'z')")
    # ASC orders NULLs last: k=1 z, k=2 x, k=NULL y.
    assert run(storage, session, "SELECT array_agg(v ORDER BY k) FROM n").rows == [
        (["z", "x", "y"],)
    ]


def test_order_by_nulls_first_on_desc(storage, session):
    run(storage, session, "CREATE TABLE n (id int PRIMARY KEY, k int, v text)")
    run(storage, session, "INSERT INTO n VALUES (1,2,'x'),(2,NULL,'y'),(3,1,'z')")
    # DESC orders NULLs first: NULL y, k=2 x, k=1 z.
    assert run(storage, session, "SELECT array_agg(v ORDER BY k DESC) FROM n").rows == [
        (["y", "x", "z"],)
    ]


def test_string_agg_skips_null_values(storage, session):
    run(storage, session, "CREATE TABLE n (id int PRIMARY KEY, k int, v text)")
    run(storage, session, "INSERT INTO n VALUES (1,1,'a'),(2,2,NULL),(3,3,'c')")
    # NULL values are skipped in the join, but ordering by k still applies.
    assert run(storage, session, "SELECT string_agg(v, ',' ORDER BY k) FROM n").rows == [("a,c",)]


# -- unordered still works ---------------------------------------------------- #


def test_plain_array_agg_unaffected(emp, session):
    rows = run(
        emp, session, "SELECT dept, array_agg(name) FROM emp WHERE dept = 'b' GROUP BY dept"
    ).rows
    assert rows == [("b", ["zed", "amy"])]


def test_ordered_array_agg_inside_derived_table(emp, session):
    # An ordered array_agg inside a materialized derived table must be finished
    # (sorted + extracted) there too — not leak the {v, k} push pairs.
    rows = run(
        emp,
        session,
        "SELECT dept, names FROM "
        "(SELECT dept, array_agg(name ORDER BY hired) AS names FROM emp GROUP BY dept) sub "
        "ORDER BY dept",
    ).rows
    assert rows == [("a", ["alice", "bob", "carol"]), ("b", ["zed", "amy"])]
