"""A twenty-fifth sweep — `array_agg` and the difference between "no rows" and
"a row with no value".

`array_agg` over zero contributing rows answered `{}` where PostgreSQL answers
NULL, and an unmatched outer-join row answered `{}` where PostgreSQL answers
`{NULL}`. A caller cannot recover from either: `{}` is itself a legal value.

**Both halves had to land together**, which is why this was recorded rather
than half-fixed when it was found. `$push` of a MISSING field pushes nothing,
so the pushed array cannot tell "no rows" from "one row with no value" — the
value is wrapped in `$ifNull` so a missing field leaves an explicit null
element behind, and only then can an empty array safely mean NULL. Doing just
the projection half would have turned the unmatched-row case from `{}` into
NULL, where PostgreSQL wants `{NULL}` — one wrong answer traded for another.

**The wrap is per-aggregate, because the sibling aggregates disagree.**
`array_agg` / `json_agg` / `jsonb_agg` KEEP null elements; `string_agg` SKIPS
them and is correct to answer NULL for a group of nothing but nulls. They share
the `$push` sites, so the change is keyed on the flag that already marked the
NULL-keeping ones.

Also fixed: `array_agg` over a JOIN reported `jsonb` where the same call over
one table reported the element's array type — the two join sites hardcoded the
json tag.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s25"))
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
    conn.execute("CREATE TABLE aa (id int PRIMARY KEY, v int, s text)")
    conn.execute("CREATE TABLE ab (id int PRIMARY KEY)")
    conn.execute("INSERT INTO aa VALUES (1,5,'p'),(2,NULL,NULL),(3,7,'q')")
    conn.execute("INSERT INTO ab VALUES (1),(2),(3)")
    return conn


def one(c, sql):
    return c.execute(sql).fetchone()[0]


# --- empty means NULL, not {} ------------------------------------------------- #


def test_no_rows_is_null(seeded):
    assert one(seeded, "SELECT array_agg(v) FROM aa WHERE false") is None


def test_a_filter_matching_nothing_is_null(seeded):
    assert one(seeded, "SELECT array_agg(v) FILTER (WHERE id > 99) FROM aa") is None


def test_json_agg_over_no_rows_is_null(seeded):
    assert one(seeded, "SELECT json_agg(v) FROM aa WHERE false") is None


# --- a row with no value still leaves an element ------------------------------ #


def test_a_null_valued_row_gives_a_null_element(seeded):
    assert one(seeded, "SELECT array_agg(v) FROM aa WHERE id = 2") == [None]


def test_an_unmatched_outer_join_row_gives_a_null_element(seeded):
    """The case the two halves exist for: row 3 of `ab` matches nothing, so the
    join gives it a row whose `a.v` key is ABSENT — not null."""
    assert seeded.execute(
        "SELECT b.id, array_agg(a.v) FROM ab b LEFT JOIN aa a "
        "ON a.id = b.id AND a.id < 3 GROUP BY b.id ORDER BY b.id"
    ).fetchall() == [(1, [5]), (2, [None]), (3, [None])]


def test_nulls_are_kept_among_real_values(seeded):
    assert one(seeded, "SELECT array_agg(v) FROM aa") == [5, None, 7]
    assert one(seeded, "SELECT array_agg(s) FROM aa") == ["p", None, "q"]


# --- string_agg disagrees, and must keep disagreeing -------------------------- #


def test_string_agg_skips_nulls(seeded):
    assert one(seeded, "SELECT string_agg(s, ',') FROM aa") == "p,q"


def test_string_agg_of_only_nulls_is_null(seeded):
    """`array_agg` answers `{NULL}` for the same group — the two aggregates
    share the push machinery and must not share this rule."""
    assert one(seeded, "SELECT string_agg(s, ',') FROM aa WHERE id = 2") is None
    assert one(seeded, "SELECT array_agg(v) FROM aa WHERE id = 2") == [None]


def test_string_agg_over_no_rows_is_null(seeded):
    assert one(seeded, "SELECT string_agg(s, ',') FROM aa WHERE false") is None


# --- ordering, DISTINCT and types --------------------------------------------- #


def test_ordered_and_distinct_keep_the_null(seeded):
    assert one(seeded, "SELECT array_agg(v ORDER BY id DESC) FROM aa") == [7, None, 5]
    assert one(seeded, "SELECT array_agg(DISTINCT v) FROM aa") == [5, 7, None]


def test_array_agg_over_a_join_types_by_element(seeded):
    """It reported jsonb where the same call over one table reported int[]."""
    got = (
        seeded.execute(
            "SELECT array_agg(a.v) FROM ab b LEFT JOIN aa a ON a.id = b.id GROUP BY b.id"
        )
        .description[0]
        .type_code
    )
    assert got == 1007


def test_array_agg_over_one_table_still_types_by_element(seeded):
    assert seeded.execute("SELECT array_agg(v) FROM aa").description[0].type_code == 1007
    assert seeded.execute("SELECT array_agg(s) FROM aa").description[0].type_code == 1009
