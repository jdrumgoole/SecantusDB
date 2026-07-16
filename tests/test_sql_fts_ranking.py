"""Full-text search ranking (#126): websearch_to_tsquery, and ranked search via
ORDER BY on a ts_rank output alias (the general ORDER-BY-output-alias fix). The
existing FTS surface (to_tsvector / to_tsquery / ts_rank / ts_headline) lives in
tests/test_sql_fts.py.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"

_STORAGES: list = []


def _new_storage():
    import tempfile

    d = tempfile.mkdtemp()
    st = Storage(d)
    _STORAGES.append((st, d))
    return st


@pytest.fixture(autouse=True)
def _close_storages():
    import shutil

    yield
    while _STORAGES:
        st, d = _STORAGES.pop()
        st.close()
        shutil.rmtree(d, ignore_errors=True)


def _fresh():
    st = _new_storage()
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE doc (id int, body text)", session=sess)
    run_sql(
        st,
        DB,
        "INSERT INTO doc VALUES "
        "(1, 'the quick brown fox'), "
        "(2, 'quick quick fox runs'), "
        "(3, 'lazy dog sleeps')",
        session=sess,
    )
    return st, sess


def _ids(st, sess, sql):
    return [r[0] for r in run_sql(st, DB, sql, session=sess)[-1].rows]


# --------------------------------------------------------------------------- #
# websearch_to_tsquery
# --------------------------------------------------------------------------- #


def _match(st, sess, query):
    sql = (
        f"SELECT id FROM doc WHERE to_tsvector(body) @@ websearch_to_tsquery('{query}') ORDER BY id"
    )
    return _ids(st, sess, sql)


def test_websearch_and():
    st, sess = _fresh()
    assert _match(st, sess, "quick fox") == [1, 2]


def test_websearch_or():
    st, sess = _fresh()
    assert _match(st, sess, "fox or dog") == [1, 2, 3]


def test_websearch_negation():
    st, sess = _fresh()
    # quick, but not runs -> doc 1 (doc 2 has "runs").
    assert _match(st, sess, "quick -runs") == [1]


def test_websearch_phrase():
    st, sess = _fresh()
    # "quick fox" as an adjacent phrase -> only doc 2 (doc 1 has "brown" between).
    assert _match(st, sess, '"quick fox"') == [2]


def test_websearch_empty():
    st, sess = _fresh()
    # A stop-word-only / empty query matches nothing.
    assert _match(st, sess, "the") == []


# --------------------------------------------------------------------------- #
# Ranked search: ORDER BY a ts_rank output alias
# --------------------------------------------------------------------------- #


def test_ranked_search_orders_by_rank_alias():
    st, sess = _fresh()
    rows = run_sql(
        st,
        DB,
        "SELECT id, ts_rank(to_tsvector(body), to_tsquery('quick')) AS rank "
        "FROM doc WHERE to_tsvector(body) @@ to_tsquery('quick') ORDER BY rank DESC",
        session=sess,
    )[-1].rows
    # Doc 2 has two 'quick' occurrences -> ranks above doc 1.
    assert [r[0] for r in rows] == [2, 1]
    assert rows[0][1] >= rows[1][1]


def test_order_by_computed_alias_general():
    # The ORDER-BY-output-alias fix is general, not FTS-specific.
    st = _new_storage()
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int, v int)", session=sess)
    run_sql(st, DB, "INSERT INTO t VALUES (1, 10), (2, 30), (3, 20)", session=sess)
    rows = run_sql(st, DB, "SELECT id, v * 2 AS d FROM t ORDER BY d DESC", session=sess)[-1].rows
    assert rows == [(2, 60), (3, 40), (1, 20)]


def test_order_by_plain_column_alias_pushdown():
    # A plain-column output alias (SELECT a AS s … ORDER BY s) on the simple
    # pushdown path (no computed columns → no evaluated path) resolves to its input
    # column. Previously raised 42703.
    st = _new_storage()
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int, a int)", session=sess)
    run_sql(st, DB, "INSERT INTO t VALUES (1, 3), (2, 1), (3, 2)", session=sess)
    asc = run_sql(st, DB, "SELECT a AS s FROM t ORDER BY s", session=sess)[-1].rows
    assert asc == [(1,), (2,), (3,)]
    desc = run_sql(st, DB, "SELECT a AS s FROM t ORDER BY s DESC", session=sess)[-1].rows
    assert desc == [(3,), (2,), (1,)]


def test_order_by_alias_real_column_wins():
    # Postgres precedence: a real input column of the alias's name wins over the
    # output alias. `ORDER BY a` orders by the real column a, not `b AS a`.
    st = _new_storage()
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int, a int, b int)", session=sess)
    run_sql(st, DB, "INSERT INTO t VALUES (1, 3, 1), (2, 1, 2), (3, 2, 3)", session=sess)
    rows = run_sql(st, DB, "SELECT a AS s, b AS a FROM t ORDER BY a", session=sess)[-1].rows
    # ordered by real column a (3,1,2) ascending: id2(a1,b2), id3(a2,b3), id1(a3,b1)
    assert rows == [(1, 2), (2, 3), (3, 1)]
