"""A seventeenth differential sweep — full-text search.

**The first corpus for this said 23 of 27 shapes diverged, and that was wrong.**
It wrote `to_tsvector(...)::text` everywhere to normalise the output — and
`tsvector::text` was itself one of the bugs, rendering the internal dict as
`{"tsvector": {"fat": [2]}}` instead of `'fat':2`. Every line inherited it.
Re-run without the cast, the real count was 13, and the picture was completely
different: rendering a tsvector DIRECTLY was already correct, and a declared
`tsvector` column round-tripped fine. A normalising cast in a probe is a
hazard — it puts one code path in front of every assertion.

What was actually wrong, in one theme: **a tsvector and a tsquery are dicts
internally, so anything that did not know their type treated them as JSON.**

- `tsvector::text` / `tsquery::text` rendered the dict as JSON.
- `length(tsvector)` measured the JSON string: 45 for a two-lexeme vector.
- `tsvector || tsvector` fell into the hstore merge branch and returned only
  the RIGHT operand — half the document silently dropped — and PostgreSQL also
  shifts the second operand's positions past the first's, which a plain merge
  would not do.
- `&&` on two tsqueries dispatched to array-overlap.
- `strip`, `numnode`, `querytree`, `tsvector_to_array` and `array_to_tsvector`
  did not exist, and the new ones needed result TAGS as much as bodies: with
  the default `text` tag a correct value still reached the client as JSON.

Also fixed: `to_tsvector('simple', ...)` ignored its configuration and dropped
stop-words anyway, losing tokens the caller explicitly asked to index.

Still absent, and recorded rather than half-built: **stemming** (`running` /
`runs` / `ran` stay distinct, so `to_tsquery('english','quick')` does not match
`'Running quickly'`), `setweight` (the representation carries no per-lexeme
weight), and `!!` as tsquery negation.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s17"))
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


def one(c, sql):
    return c.execute(sql).fetchone()[0]


# --- the dict leaked as JSON wherever the type was not known ----------------- #


def test_tsvector_cast_to_text(conn):
    assert one(conn, "SELECT 'a fat cat'::tsvector::text") == "'a' 'cat' 'fat'"


def test_tsquery_cast_to_text(conn):
    assert one(conn, "SELECT 'fat & cat'::tsquery::text") == "'fat' & 'cat'"


def test_tsvector_column_cast_to_text(conn):
    conn.execute("CREATE TABLE tv (id int, v tsvector)")
    conn.execute("INSERT INTO tv VALUES (1, to_tsvector('english','a fat cat'))")
    assert one(conn, "SELECT v::text FROM tv") == "'cat':3 'fat':2"


def test_length_counts_lexemes_not_json(conn):
    """It returned 45 — the length of the internal dict's JSON."""
    assert one(conn, "SELECT length(to_tsvector('simple','quick brown quick'))") == 2
    assert one(conn, "SELECT length(''::tsvector)") == 0


# --- || dropped half the document -------------------------------------------- #


def test_tsvector_concat_keeps_both_sides_and_shifts_positions(conn):
    assert one(
        conn, "SELECT (to_tsvector('simple','a b') || to_tsvector('simple','c d'))::text"
    ) == ("'a':1 'b':2 'c':3 'd':4")


def test_tsvector_concat_type(conn):
    r = conn.execute("SELECT to_tsvector('simple','a') || to_tsvector('simple','b')")
    assert r.description[0].type_code == 3614
    assert r.fetchone()[0] == "'a':1 'b':2"


def test_tsquery_or_and_and_operators(conn):
    assert one(conn, "SELECT (to_tsquery('simple','a') || to_tsquery('simple','b'))::text") == (
        "'a' | 'b'"
    )
    assert one(conn, "SELECT (to_tsquery('simple','a') && to_tsquery('simple','b'))::text") == (
        "'a' & 'b'"
    )


# --- the new functions ------------------------------------------------------- #


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("SELECT strip(to_tsvector('simple','quick brown quick'))::text", "'brown' 'quick'"),
        ("SELECT querytree(to_tsquery('simple','quick & brown'))", "'quick' & 'brown'"),
        # A purely negative query selects nothing on its own, and PostgreSQL
        # renders that as T rather than as the query.
        ("SELECT querytree(to_tsquery('simple','!quick'))", "T"),
        ("SELECT array_to_tsvector(ARRAY['b','a'])::text", "'a' 'b'"),
    ],
)
def test_new_functions(conn, sql, expected):
    assert one(conn, sql) == expected


@pytest.mark.parametrize(
    "query,nodes",
    [("quick", 1), ("quick & brown", 3), ("!quick", 2), ("quick & brown | fox", 5)],
)
def test_numnode_counts_operators_too(conn, query, nodes):
    assert one(conn, f"SELECT numnode(to_tsquery('simple','{query}'))") == nodes


def test_tsvector_to_array(conn):
    assert one(conn, "SELECT tsvector_to_array(to_tsvector('simple','quick brown'))") == [
        "brown",
        "quick",
    ]
    assert one(conn, "SELECT tsvector_to_array(''::tsvector)") == []


def test_the_new_functions_report_their_types(conn):
    """The bodies were only half of it: with the default `text` tag a correct
    value still reached the client as JSON."""
    for sql, oid in [
        ("SELECT strip(to_tsvector('simple','a'))", 3614),
        ("SELECT array_to_tsvector(ARRAY['a'])", 3614),
        ("SELECT numnode(to_tsquery('simple','a'))", 23),
        ("SELECT querytree(to_tsquery('simple','a'))", 25),
        ("SELECT tsvector_to_array(to_tsvector('simple','a'))", 1009),
    ]:
        assert conn.execute(sql).description[0].type_code == oid, sql


# --- the text-search configuration ------------------------------------------- #


def test_simple_config_keeps_stop_words(conn):
    assert one(conn, "SELECT to_tsvector('simple', 'The quick brown fox')::text") == (
        "'brown':3 'fox':4 'quick':2 'the':1"
    )


def test_english_config_drops_them(conn):
    assert one(conn, "SELECT to_tsvector('english', 'The quick brown fox')::text") == (
        "'brown':3 'fox':4 'quick':2"
    )


# --- regression cover -------------------------------------------------------- #


def test_matching_still_works(conn):
    assert (
        one(
            conn,
            "SELECT to_tsvector('english','the quick brown fox') @@ to_tsquery('english','quick')",
        )
        is True
    )
    assert (
        one(
            conn,
            "SELECT to_tsvector('english','the quick brown fox') @@ to_tsquery('english','slow')",
        )
        is False
    )


VEC = "to_tsvector('english','quick brown')"


@pytest.mark.parametrize(
    "query,expected",
    [("quick <-> brown", True), ("brown <-> quick", False), ("qui:*", True)],
)
def test_phrase_and_prefix_queries_still_work(conn, query, expected):
    assert one(conn, f"SELECT {VEC} @@ to_tsquery('english','{query}')") is expected
    assert (
        one(conn, "SELECT to_tsvector('english','quick brown') @@ to_tsquery('english','qui:*')")
        is True
    )


def test_a_declared_tsvector_column_round_trips(conn):
    conn.execute("CREATE TABLE tv2 (id int, v tsvector, q tsquery)")
    conn.execute(
        "INSERT INTO tv2 VALUES "
        "(1, to_tsvector('english','a fat cat'), to_tsquery('english','fat & cat'))"
    )
    r = conn.execute("SELECT v, q FROM tv2")
    assert [d.type_code for d in r.description] == [3614, 3615]
    assert r.fetchone() == ("'cat':3 'fat':2", "'fat' & 'cat'")
