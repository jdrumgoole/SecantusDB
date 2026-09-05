"""A twenty-third sweep — the hypothetical-set aggregates.

`rank(v) WITHIN GROUP (ORDER BY expr)` asks what `v` *would* rank if it were
inserted into the group; `dense_rank`, `percent_rank` and `cume_dist` are the
same question with different arithmetic. All four were `0A000`.

They share the ordered-set plumbing that `percentile_cont` / `percentile_disc`
/ `mode` already used — push the ORDER BY values, finish in Python — and
needed only a different payload and a different finish.

**The sort direction is part of the answer, and NULLs take part in it.** That
is what makes this more than a comparison: on the same data `rank(20) ORDER BY
v` is 2 and `rank(20) ORDER BY v DESC` is 3, because DESC defaults to NULLS
FIRST and the NULL row then sorts *before* the hypothetical. Encoding NULLs as
an extreme value and letting the direction flip the comparison gets `DESC` and
`DESC NULLS LAST` exactly backwards — reversing the comparison also reverses
where the NULLs went — which is what the first version of this did. The
null-bucket is now absolute and only the value comparison flips.

Two more measured details: `percent_rank` and `cume_dist` use **different
denominators** (`N` and `N + 1`), and cume_dist counts the hypothetical row
itself while percent_rank does not. And on an EMPTY group the four answer
1, 1, 0.0 and 1.0 — not NULL.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")

#: v = 10, 20, 20, 30, NULL — a tie, a gap, and a NULL.
SEED = "(1,10,'a'),(2,20,'a'),(3,20,'a'),(4,30,'b'),(5,NULL,'b')"


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s23"))
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
    conn.execute("CREATE TABLE hs (id int PRIMARY KEY, v int, g text)")
    conn.execute(f"INSERT INTO hs VALUES {SEED}")
    return conn


def one(c, sql):
    return c.execute(sql).fetchone()[0]


@pytest.mark.parametrize(
    "expr,expected",
    [
        ("rank(20)", 2),
        ("dense_rank(20)", 2),
        ("percent_rank(20)", 0.2),
        ("cume_dist(20)", 0.6666666666666666),
        # Below everything, above everything, and between two values.
        ("rank(5)", 1),
        ("rank(99)", 5),
        ("dense_rank(25)", 3),
        ("cume_dist(25)", 0.6666666666666666),
        ("percent_rank(30)", 0.6),
    ],
)
def test_ascending(seeded, expr, expected):
    got = one(seeded, f"SELECT {expr} WITHIN GROUP (ORDER BY v) FROM hs")
    assert got == pytest.approx(expected)


@pytest.mark.parametrize(
    "order,expected",
    [
        # ASC defaults to NULLS LAST, DESC to NULLS FIRST — so the NULL row is
        # before the hypothetical in one and after it in the other.
        ("v", 2),
        ("v DESC", 3),
        ("v NULLS FIRST", 3),
        ("v DESC NULLS LAST", 2),
    ],
)
def test_direction_and_nulls_change_the_rank(seeded, order, expected):
    assert one(seeded, f"SELECT rank(20) WITHIN GROUP (ORDER BY {order}) FROM hs") == expected


def test_a_null_hypothetical_sorts_with_the_nulls(seeded):
    """Under ASC NULLS LAST every real value is before it."""
    assert one(seeded, "SELECT rank(NULL) WITHIN GROUP (ORDER BY v) FROM hs") == 5


def test_empty_group_is_not_null(seeded):
    assert one(seeded, "SELECT rank(20) WITHIN GROUP (ORDER BY v) FROM hs WHERE false") == 1
    assert one(seeded, "SELECT dense_rank(20) WITHIN GROUP (ORDER BY v) FROM hs WHERE false") == 1
    assert (
        one(seeded, "SELECT percent_rank(20) WITHIN GROUP (ORDER BY v) FROM hs WHERE false") == 0.0
    )
    assert one(seeded, "SELECT cume_dist(20) WITHIN GROUP (ORDER BY v) FROM hs WHERE false") == 1.0


def test_grouped(seeded):
    assert seeded.execute(
        "SELECT g, rank(20) WITHIN GROUP (ORDER BY v) FROM hs GROUP BY g ORDER BY g"
    ).fetchall() == [("a", 2), ("b", 1)]


def test_text_ordering(seeded):
    assert one(seeded, "SELECT rank('b') WITHIN GROUP (ORDER BY g) FROM hs") == 4


def test_result_types(seeded):
    """rank / dense_rank are int8; percent_rank / cume_dist are float8."""
    for expr, oid in [
        ("rank(20)", 20),
        ("dense_rank(20)", 20),
        ("percent_rank(20)", 701),
        ("cume_dist(20)", 701),
    ]:
        got = (
            seeded.execute(f"SELECT {expr} WITHIN GROUP (ORDER BY v) FROM hs")
            .description[0]
            .type_code
        )
        assert got == oid, f"{expr}: {got}"


def test_the_existing_ordered_set_aggregates_still_work(seeded):
    """They share the plumbing, so they are the regression risk."""
    assert one(seeded, "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY v) FROM hs") == 20.0
    assert one(seeded, "SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY v) FROM hs") == 20
    assert one(seeded, "SELECT mode() WITHIN GROUP (ORDER BY v) FROM hs") == 20


def test_multi_column_is_refused_rather_than_guessed(seeded):
    """PostgreSQL takes one argument per ORDER BY expression; we serve the
    single-column form and refuse the rest instead of answering something
    plausible."""
    with pytest.raises(psycopg.Error) as ei:
        seeded.execute("SELECT rank(20, 'a') WITHIN GROUP (ORDER BY v, g) FROM hs")
    assert getattr(ei.value.diag, "sqlstate", None) == "0A000"
