# ruff: noqa: E501 -- the cases are verbatim SQL, one per line.
"""Full-text search on the Python PG server, pinned to PostgreSQL 15.19.

Every expected value below is what PostgreSQL 15.19 printed for
``SELECT (<expr>)::text``, measured 2026-10-01 -- not derived from the
implementation. The non-English configurations need the ``snowballstemmer``
package (2.2.0, the Snowball release PostgreSQL 15 ships) and skip without it.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"

ENGLISH_CASES = [
    ("to_tsvector('english', 'The cats were running quickly')", "'cat':2 'quick':5 'run':4"),
    (
        "to_tsvector('english', 'state-of-the-art multi-level')",
        "'art':5 'level':8 'multi':7 'multi-level':6 'state':2 'state-of-the-art':1",
    ),
    (
        "to_tsvector('english', 'joe@example.com visited example.com and http://foo.org/x/y')",
        "'/x/y':7 'example.com':3 'foo.org':6 'foo.org/x/y':5 'joe@example.com':1 'visit':2",
    ),
    (
        "to_tsvector('english', 'v1.2.3 is 3.5 times 1e10 or -42')",
        "'-42':7 '1e10':5 '3.5':3 'time':4 'v1.2.3':1",
    ),
    ("to_tsvector('english', 'naïve café — «quoted» x²')", "'café':2 'naïv':1 'quot':3 'x':4"),
    ("to_tsvector('simple', 'The Quick brown')", "'brown':3 'quick':2 'the':1"),
    ("'a:1 a:1 b:5,3,3'::tsvector", "'a':1 'b':3,5"),
    ("'a:20000'::tsvector", "'a':16383"),
    ("$$'\\x' 'it''s' 'a b'$$::tsvector", "'a b' 'it''s' 'x'"),
    (
        "setweight(to_tsvector('english', 'cat sat mat'), 'B', ARRAY['cat', 'mat'])",
        "'cat':1B 'mat':3B 'sat':2",
    ),
    (
        "setweight('a:1,2 b:3'::tsvector, 'c') || setweight('c:1'::tsvector, 'a')",
        "'a':1C,2C 'b':3C 'c':4A",
    ),
    ("ts_delete(to_tsvector('english', 'cat sat mat'), ARRAY['sat', 'cat'])", "'mat':3"),
    ("ts_filter('a:1A b:2B c:3'::tsvector, '{a,b}')", "'a':1A 'b':2B"),
    ("'fat:*AB & !rat:C'::tsquery", "'fat':*AB & !'rat':C"),
    ("'a & (b | c) & d'::tsquery", "'a' & ( 'b' | 'c' ) & 'd'"),
    ("'(a | b) <-> c'::tsquery", "( 'a' | 'b' ) <-> 'c'"),
    ("'!(a & b)'::tsquery", "!( 'a' & 'b' )"),
    ("to_tsquery('english', 'state-of-the-art')", "'state-of-the-art' <-> 'state' <3> 'art'"),
    ("to_tsquery('english', '''supernovae stars'' & !crab')", "'supernova' <-> 'star' & !'crab'"),
    ("to_tsquery('english', 'fat:ab & cats:*')", "'fat':AB & 'cat':*"),
    (
        "plainto_tsquery('english', 'State-of-the-art 42 ideas!')",
        "'state-of-the-art' & 'state' & 'art' & '42' & 'idea'",
    ),
    ("phraseto_tsquery('english', 'the quick and the dead')", "'quick' <3> 'dead'"),
    (
        "websearch_to_tsquery('english', '\"cat dog\" or -\"fish bowl\"')",
        "'cat' <-> 'dog' | !( 'fish' <-> 'bowl' )",
    ),
    ("tsquery_phrase('a'::tsquery, 'b'::tsquery, 3)", "'a' <3> 'b'"),
    ("querytree('a & !b | c'::tsquery)", "'a' | 'c'"),
    ("numnode('a & !b | c'::tsquery)", "6"),
    ("to_tsvector('english', 'cat bat mat') @@ to_tsquery('english', 'cat <-> !sat')", "true"),
    (
        "to_tsvector('english', 'cat sat mat') @@ to_tsquery('english', '(cat | dog) <-> sat')",
        "true",
    ),
    (
        "setweight(to_tsvector('english', 'cat sat'), 'A') @@ to_tsquery('english', 'cat:B')",
        "false",
    ),
    (
        "ts_rank(to_tsvector('english', 'The quick brown fox jumps over the lazy dog'), to_tsquery('english', 'fox & dog'))",
        "0.09148999",
    ),
    (
        "ts_rank(to_tsvector('english', 'fox fox fox dog'), to_tsquery('english', 'fox <-> dog'))",
        "0.26691276",
    ),
    (
        "ts_rank('{0.1, 0.2, 0.4, 1.0}', to_tsvector('english', 'fox dog'), to_tsquery('english', 'fox'))",
        "0.06079271",
    ),
    (
        "ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox & dog'))",
        "0.35",
    ),
    (
        "ts_rank_cd(to_tsvector('english', 'fox dog fox cat dog fox'), to_tsquery('english', 'fox & dog'), 4)",
        "0.20416667",
    ),
    (
        "ts_headline('english', 'The quick brown fox jumps', to_tsquery('english', 'fox'))",
        "The quick brown <b>fox</b> jumps",
    ),
]

SNOWBALL_CASES = [
    (
        "to_tsvector('french', 'Les chats mangeaient des souris dans la maison abandonnée')",
        "'abandon':9 'chat':2 'le':1 'maison':8 'mang':3 'sour':5",
    ),
    (
        "to_tsvector('german', 'Die Katzen fraßen Mäuse in den verlassenen Häusern')",
        "'frass':3 'haus':8 'katz':2 'maus':4 'verlass':7",
    ),
    (
        "to_tsvector('russian', 'Кошки ели мышей в заброшенных домах running')",
        "'run':7 'дом':6 'ел':2 'заброшен':5 'кошк':1 'мыш':3",
    ),
    (
        "to_tsvector('spanish', 'Los gatos comían ratones en las casas abandonadas')",
        "'abandon':8 'cas':7 'com':3 'gat':2 'raton':4",
    ),
    (
        "websearch_to_tsquery('german', 'katzen -hunde \"alte häuser\"')",
        "'katz' & !'hund' & 'alt' <-> 'haus'",
    ),
]

ERROR_CASES = [
    ("'a & '::tsquery", ("42601", 'no operand in tsquery: "a & "')),
    ("'& a'::tsquery", ("42601", 'syntax error in tsquery: "& a"')),
    ("'a:0'::tsvector", ("42601", 'wrong position info in tsvector: "a:0"')),
    (
        "to_tsquery('nosuch', 'chat')",
        ("42704", 'text search configuration "nosuch" does not exist'),
    ),
    ("array_to_tsvector(ARRAY['a', NULL])", ("22004", "lexeme array may not contain nulls")),
    ("array_to_tsvector(ARRAY['a', ''])", ("2200F", "lexeme array may not contain empty strings")),
]


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def _text(storage, expr):
    res = run_sql(storage, DB, f"SELECT ({expr})::text", session=Session(database=DB))
    return res[-1].rows[0][0]


@pytest.mark.parametrize(("expr", "expected"), ENGLISH_CASES)
def test_matches_postgresql(storage, expr, expected):
    assert _text(storage, expr) == expected


@pytest.mark.parametrize(("expr", "expected"), SNOWBALL_CASES)
def test_snowball_configurations(storage, expr, expected):
    pytest.importorskip("snowballstemmer")
    assert _text(storage, expr) == expected


@pytest.mark.parametrize(("expr", "expected"), ERROR_CASES)
def test_errors_match_postgresql(storage, expr, expected):
    with pytest.raises(errors.SQLError) as info:
        _text(storage, expr)
    assert (info.value.sqlstate, info.value.message) == expected
