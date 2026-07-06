"""Full-text search ranking (#126): websearch_to_tsquery, and ranked search via
ORDER BY on a ts_rank output alias (the general ORDER-BY-output-alias fix). The
existing FTS surface (to_tsvector / to_tsquery / ts_rank / ts_headline) lives in
tests/test_sql_fts.py.
"""

from __future__ import annotations

from secantus.sql import run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "d"


def _fresh():
    st = FakeStorage()
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
    st = FakeStorage()
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int, v int)", session=sess)
    run_sql(st, DB, "INSERT INTO t VALUES (1, 10), (2, 30), (3, 20)", session=sess)
    rows = run_sql(st, DB, "SELECT id, v * 2 AS d FROM t ORDER BY d DESC", session=sess)[-1].rows
    assert rows == [(2, 60), (3, 40), (1, 20)]
