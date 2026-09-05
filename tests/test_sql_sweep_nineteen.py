"""A nineteenth differential sweep — stemming, the largest gap in text search.

Until now `to_tsvector('english', …)` did not stem, so `cats` did not match
`cat` and `to_tsquery('english','quick')` did not find a row whose title is
`'Running quickly'`. That is the worst class of text-search defect: **a query
that should match returns nothing**, silently.

`secantus.sql.snowball` now implements the English (Porter2) algorithm
PostgreSQL's `english` configuration uses, written out rather than taken from a
dependency — SecantusDB ships self-contained wheels, and a stemmer is a closed,
fully specified algorithm. `tests/test_sql_stemmer.py` pins it against 6,094
words stemmed by PostgreSQL itself.

Both sides of a search stem, which is the point: the query's `running` and the
document's `runs` have to meet at `run`. Prefixes stem too (`running:*` renders
as `'run':*`), and `to_tsquery` now drops stop-words the way the document side
always did — leaving them in produced a query that could never match, since no
document indexes them.

`simple` still neither stems nor drops stop-words, which is what makes it
`simple`.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s19"))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    host, port = srv.address
    c = psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True)
    try:
        yield c
    finally:
        c.close()
        srv.stop()
        st.close()


@pytest.fixture
def seeded(conn):
    conn.execute("CREATE TABLE ts1 (id int PRIMARY KEY, title text, body text)")
    conn.execute(
        "INSERT INTO ts1 VALUES "
        "(1,'The quick brown fox','jumps over the lazy dog'),"
        "(2,'Databases are fun','indexing and querying text'),"
        "(3,'Running quickly','runs ran running runner')"
    )
    return conn


def one(c, sql):
    return c.execute(sql).fetchone()[0]


# --- the document side ------------------------------------------------------- #


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("SELECT to_tsvector('english','running runs ran')::text", "'ran':3 'run':1,2"),
        ("SELECT to_tsvector('english','cats and dogs')::text", "'cat':1 'dog':3"),
        ("SELECT to_tsvector('english','the running cats')::text", "'cat':3 'run':2"),
        ("SELECT to_tsvector('english','Running RUNS')::text", "'run':1,2"),
    ],
)
def test_documents_are_stemmed(conn, sql, expected):
    assert one(conn, sql) == expected


def test_simple_neither_stems_nor_drops_stop_words(conn):
    assert one(conn, "SELECT to_tsvector('simple','the running cats')::text") == (
        "'cats':3 'running':2 'the':1"
    )


# --- the query side ---------------------------------------------------------- #


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("SELECT to_tsquery('english','running')::text", "'run'"),
        ("SELECT to_tsquery('english','runs & cats')::text", "'run' & 'cat'"),
        ("SELECT plainto_tsquery('english','running cats')::text", "'run' & 'cat'"),
        ("SELECT phraseto_tsquery('english','running cats')::text", "'run' <-> 'cat'"),
        ("SELECT websearch_to_tsquery('english','running cats')::text", "'run' & 'cat'"),
        # A prefix stems too, or it would stop matching stemmed documents.
        ("SELECT to_tsquery('english','running:*')::text", "'run':*"),
        ("SELECT to_tsquery('simple','running')::text", "'running'"),
    ],
)
def test_queries_are_stemmed(conn, sql, expected):
    assert one(conn, sql) == expected


@pytest.mark.parametrize(
    "sql",
    [
        "SELECT to_tsquery('english','the')::text",
        "SELECT plainto_tsquery('english','the and of')::text",
    ],
)
def test_stop_words_are_dropped_from_queries(conn, sql):
    """Leaving them in produced a query that can never match: no document
    indexes a stop-word."""
    assert one(conn, sql) == ""


def test_stop_words_are_pruned_without_losing_their_neighbours(conn):
    assert one(conn, "SELECT to_tsquery('english','the & quick')::text") == "'quick'"
    assert one(conn, "SELECT to_tsquery('english','quick & the')::text") == "'quick'"


def test_simple_keeps_stop_words_in_a_query(conn):
    assert one(conn, "SELECT to_tsquery('simple','the')::text") == "'the'"


@pytest.mark.parametrize(
    "query,expected",
    [
        # A term dropped from the MIDDLE of a phrase WIDENS the gap between its
        # neighbours rather than closing it — `brown` is still two tokens after
        # `quick` in any document that matches, so closing the gap would
        # silently match a different set of documents.
        ("quick <-> the <-> brown", "'quick' <2> 'brown'"),
        ("quick <-> the <-> the <-> brown", "'quick' <3> 'brown'"),
        # A term dropped from the START owes nothing: there is no earlier term
        # for the gap to apply to.
        ("the <-> quick", "'quick'"),
        ("quick <-> the", "'quick'"),
        ("a <-> b", "'b'"),
        ("a <3> b", "'b'"),
    ],
)
def test_a_pruned_phrase_term_widens_the_gap(conn, query, expected):
    assert one(conn, f"SELECT to_tsquery('english','{query}')::text") == expected


def test_simple_keeps_the_whole_phrase(conn):
    assert one(conn, "SELECT to_tsquery('simple','a <-> b')::text") == "'a' <-> 'b'"


# --- the reason any of this matters ------------------------------------------ #


def test_a_query_now_finds_a_differently_inflected_row(seeded):
    """Row 3's title is 'Running quickly'. Before stemming this returned only
    row 1 — a silent miss, which is the worst way for a search to be wrong."""
    assert seeded.execute(
        "SELECT id FROM ts1 WHERE to_tsvector('english', title) "
        "@@ to_tsquery('english','quick') ORDER BY id"
    ).fetchall() == [(1,), (3,)]


def test_matching_across_inflections_both_ways(seeded):
    assert seeded.execute(
        "SELECT id FROM ts1 WHERE to_tsvector('english', body) "
        "@@ to_tsquery('english','run') ORDER BY id"
    ).fetchall() == [(3,)]
    assert seeded.execute(
        "SELECT id FROM ts1 WHERE to_tsvector('english', body) "
        "@@ plainto_tsquery('english','lazy dog') ORDER BY id"
    ).fetchall() == [(1,)]


def test_phrase_and_prefix_queries_still_work(conn):
    vec = "to_tsvector('english','quick brown')"
    assert one(conn, f"SELECT {vec} @@ to_tsquery('english','quick <-> brown')") is True
    assert one(conn, f"SELECT {vec} @@ to_tsquery('english','brown <-> quick')") is False
    assert one(conn, f"SELECT {vec} @@ to_tsquery('english','qui:*')") is True
