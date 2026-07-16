"""Full-text search (#107): tsvector / tsquery types, to_tsvector /
to_tsquery / plainto_tsquery, the @@ match operator, and ts_rank.
"""

from __future__ import annotations

import pytest

from secantus.sql import fts, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


# --------------------------------------------------------------------------- #
# Pure fts.py
# --------------------------------------------------------------------------- #


def test_to_tsvector_drops_stopwords_keeps_positions():
    v = fts.to_tsvector("a fat cat sat on the mat")
    assert fts.render_tsvector(v) == "'cat':3 'fat':2 'mat':7 'sat':4"


def test_to_tsquery_operators():
    assert fts.render_tsquery(fts.to_tsquery("cat & dog")) == "'cat' & 'dog'"
    assert fts.render_tsquery(fts.to_tsquery("cat | dog")) == "'cat' | 'dog'"
    assert fts.render_tsquery(fts.to_tsquery("cat & !dog")) == "'cat' & !'dog'"


def test_to_tsquery_parens():
    assert fts.render_tsquery(fts.to_tsquery("cat & (sat | run)")) == "'cat' & ( 'sat' | 'run' )"


def test_plainto_ands_terms():
    assert fts.render_tsquery(fts.plainto_tsquery("the fat cat")) == "'fat' & 'cat'"


def test_matches_and_or_not():
    v = fts.to_tsvector("the quick brown fox")
    assert fts.matches(v, fts.to_tsquery("quick & fox")) is True
    assert fts.matches(v, fts.to_tsquery("quick & dog")) is False
    assert fts.matches(v, fts.to_tsquery("quick | dog")) is True
    assert fts.matches(v, fts.to_tsquery("quick & !dog")) is True
    assert fts.matches(v, fts.to_tsquery("quick & !fox")) is False


def test_ts_rank_monotonic():
    one = fts.to_tsvector("quick brown fox")
    two = fts.to_tsvector("quick quick fox")
    q = fts.to_tsquery("quick")
    assert fts.ts_rank(two, q) > fts.ts_rank(one, q) > 0.0
    assert fts.ts_rank(one, fts.to_tsquery("dog")) == 0.0


def test_bad_tsquery_raises():
    with pytest.raises(fts.TSQueryError):
        fts.to_tsquery("cat &")


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


@pytest.fixture
def docs(storage, session):
    run(storage, session, "CREATE TABLE docs (id int PRIMARY KEY, body tsvector)")
    run(storage, session, "INSERT INTO docs VALUES (1, to_tsvector('the quick brown fox'))")
    run(storage, session, "INSERT INTO docs VALUES (2, to_tsvector('a lazy dog sleeps'))")
    run(storage, session, "INSERT INTO docs VALUES (3, to_tsvector('the quick dog runs quick'))")
    return storage


def test_to_tsvector_typed(storage, session):
    assert col(storage, session, "SELECT to_tsvector('a fat cat')").type_tag == "tsvector"


def test_to_tsquery_typed(storage, session):
    assert col(storage, session, "SELECT to_tsquery('cat')").type_tag == "tsquery"
    assert col(storage, session, "SELECT plainto_tsquery('cat dog')").type_tag == "tsquery"


def test_match_scalar_typed_bool(storage, session):
    c = col(storage, session, "SELECT to_tsvector('a cat sat') @@ to_tsquery('cat')")
    assert c.type_tag == "bool"


def test_match_scalar_value(storage, session):
    assert val(storage, session, "SELECT to_tsvector('a cat') @@ to_tsquery('cat')") is True
    assert val(storage, session, "SELECT to_tsvector('a cat') @@ to_tsquery('dog')") is False


def test_tsvector_column_roundtrip(docs, session):
    assert val(docs, session, "SELECT body FROM docs WHERE id = 1") == {
        "tsvector": {"quick": [2], "brown": [3], "fox": [4]}
    }


def test_where_match(docs, session):
    ids = [
        r[0]
        for r in run(
            docs, session, "SELECT id FROM docs WHERE body @@ to_tsquery('quick') ORDER BY id"
        ).rows
    ]
    assert ids == [1, 3]


def test_where_match_and(docs, session):
    ids = [
        r[0]
        for r in run(
            docs, session, "SELECT id FROM docs WHERE body @@ to_tsquery('quick & dog') ORDER BY id"
        ).rows
    ]
    assert ids == [3]


def test_where_match_plainto(docs, session):
    ids = [
        r[0]
        for r in run(
            docs,
            session,
            "SELECT id FROM docs WHERE body @@ plainto_tsquery('lazy dog') ORDER BY id",
        ).rows
    ]
    assert ids == [2]


def test_ts_rank_orders_results(docs, session):
    # id 3 mentions 'quick' twice, so it outranks id 1.
    rows = run(
        docs,
        session,
        "SELECT id FROM docs WHERE body @@ to_tsquery('quick') "
        "ORDER BY ts_rank(body, to_tsquery('quick')) DESC",
    ).rows
    assert [r[0] for r in rows] == [3, 1]


def test_ts_rank_typed_float(docs, session):
    c = col(docs, session, "SELECT ts_rank(body, to_tsquery('quick')) FROM docs WHERE id = 1")
    assert c.type_tag == "float8"


def test_tsvector_cast(storage, session):
    assert val(storage, session, "SELECT 'cat sat'::tsvector") == {
        "tsvector": {"cat": [], "sat": []}
    }


def test_tsquery_cast(storage, session):
    assert val(storage, session, "SELECT 'cat & dog'::tsquery") == fts.to_tsquery("cat & dog")


# --------------------------------------------------------------------------- #
# Follow-ups (#111): prefix, phrase, phraseto_tsquery, ts_headline
# --------------------------------------------------------------------------- #


def test_prefix_query_parse_render():
    assert fts.render_tsquery(fts.to_tsquery("cat:*")) == "'cat':*"


def test_prefix_match():
    v = fts.to_tsvector("the category is nice")
    assert fts.matches(v, fts.to_tsquery("cat:*")) is True
    assert fts.matches(fts.to_tsvector("the dog runs"), fts.to_tsquery("cat:*")) is False


def test_phrase_adjacency():
    v = fts.to_tsvector("the quick brown fox")
    assert fts.matches(v, fts.to_tsquery("quick <-> brown")) is True
    assert fts.matches(fts.to_tsvector("brown quick fox"), fts.to_tsquery("quick <-> brown")) is (
        False
    )


def test_phrase_distance():
    # quick@1 fox@3 -> distance 2 (a stop-word 'the' widens the gap in the doc).
    assert fts.matches(fts.to_tsvector("quick brown fox"), fts.to_tsquery("quick <2> fox")) is True
    assert fts.matches(fts.to_tsvector("quick brown fox"), fts.to_tsquery("quick <-> fox")) is False


def test_phrase_render():
    assert fts.render_tsquery(fts.to_tsquery("a <-> b")) == "'a' <-> 'b'"
    assert fts.render_tsquery(fts.to_tsquery("a <3> b")) == "'a' <3> 'b'"


def test_phraseto_tsquery():
    q = fts.phraseto_tsquery("quick brown fox")
    assert fts.matches(fts.to_tsvector("a quick brown fox jumps"), q) is True
    assert fts.matches(fts.to_tsvector("quick fox brown"), q) is False


def test_ts_headline():
    out = fts.ts_headline("The quick brown fox", fts.to_tsquery("quick | fox"))
    assert out == "The <b>quick</b> brown <b>fox</b>"


def test_ts_headline_prefix():
    out = fts.ts_headline("categories and cats", fts.to_tsquery("cat:*"))
    assert out == "<b>categories</b> and <b>cats</b>"


def test_prefix_query_typed(storage, session):
    assert col(storage, session, "SELECT to_tsquery('cat:*')").type_tag == "tsquery"


def test_phraseto_tsquery_typed(storage, session):
    assert col(storage, session, "SELECT phraseto_tsquery('quick brown')").type_tag == "tsquery"


def test_ts_headline_typed(storage, session):
    c = col(storage, session, "SELECT ts_headline('the quick fox', to_tsquery('quick'))")
    assert c.type_tag == "text"


def test_ts_headline_value(storage, session):
    assert val(storage, session, "SELECT ts_headline('the quick fox', to_tsquery('quick'))") == (
        "the <b>quick</b> fox"
    )


def test_where_phrase(docs, session):
    ids = [
        r[0]
        for r in run(
            docs, session, "SELECT id FROM docs WHERE body @@ to_tsquery('quick & dog') ORDER BY id"
        ).rows
    ]
    assert ids == [3]


def test_where_prefix(storage, session):
    run(storage, session, "CREATE TABLE d (id int PRIMARY KEY, body tsvector)")
    run(storage, session, "INSERT INTO d VALUES (1, to_tsvector('running quickly'))")
    run(storage, session, "INSERT INTO d VALUES (2, to_tsvector('a slow walk'))")
    ids = [
        r[0]
        for r in run(
            storage, session, "SELECT id FROM d WHERE body @@ to_tsquery('run:*') ORDER BY id"
        ).rows
    ]
    assert ids == [1]
