"""A twenty-first sweep — the two-argument statistical aggregates.

`corr`, `covar_pop`, `covar_samp` and the nine `regr_*` functions were all
`0A000`. They are one feature, not twelve: every one is derived from the same
six sums (N, ΣX, ΣY, ΣX², ΣY², ΣXY), and they differ only in the finishing
arithmetic, so they share one accumulator set and one post-aggregate.

**Two things here resist derivation and were measured, not reasoned.**

PostgreSQL spells all of them `f(Y, X)` — the DEPENDENT variable first, which
is the opposite of what the names suggest: `regr_slope(y, x)` is the slope of
y on x. Getting the order backwards returns the reciprocal, silently.

And the degenerate cases do not follow one rule. With zero variation in X,
`corr` is NULL and so are `regr_slope` / `regr_intercept` / `regr_r2`. With
zero variation in **Y**, `corr` is still NULL — but **`regr_r2` is 1.0**, and
slope and intercept are ordinary numbers. A constant Y is perfectly
"explained"; a constant X explains nothing. Guessing gets one of those wrong.

`regr_count` is also the only one of the twelve defined over an empty input:
it is 0 where every other one is NULL, and it is int8 where the rest are
float8.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")

#: (x, y) = (1,2), (2,4), (3,7), (4,8), plus a row whose x is NULL — so a pair
#: contributes only when BOTH are present and `regr_count` is 4, not 5.
SEED = "(1,1,2),(2,2,4),(3,3,7),(4,4,8),(5,NULL,5)"


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s21"))
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
    conn.execute("CREATE TABLE st (id int PRIMARY KEY, x float8, y float8, g text)")
    conn.execute("CREATE TABLE stg (id int PRIMARY KEY, x float8, y float8, g text)")
    conn.execute(f"INSERT INTO st (id, x, y) VALUES {SEED}")
    conn.execute("UPDATE st SET g = CASE WHEN id < 4 THEN 'a' ELSE 'b' END")
    return conn


def one(c, sql):
    return c.execute(sql).fetchone()[0]


def close(got, want, tol=1e-12):
    return got is not None and abs(got - want) < tol


@pytest.mark.parametrize(
    "func,expected",
    [
        ("regr_count", 4),
        ("regr_avgx", 2.5),
        ("regr_avgy", 5.25),
        ("regr_sxx", 5.0),
        ("regr_syy", 22.75),
        ("regr_sxy", 10.5),
        ("covar_pop", 2.625),
        ("covar_samp", 3.5),
        ("corr", 0.9844951849708403),
        ("regr_slope", 2.1),
        ("regr_intercept", 0.0),
        ("regr_r2", 0.9692307692307692),
    ],
)
def test_each_aggregate(seeded, func, expected):
    got = one(seeded, f"SELECT {func}(y, x) FROM st")
    assert close(got, expected), f"{func}: {got!r} != {expected!r}"


def test_only_rows_with_both_arguments_count(seeded):
    """The fifth row has a NULL x, so it contributes to nothing — not to the
    count and not to the means, which have to agree about the population."""
    assert one(seeded, "SELECT regr_count(y, x) FROM st") == 4
    assert close(one(seeded, "SELECT regr_avgy(y, x) FROM st"), 5.25)
    # avg(y) over ALL five rows would be 5.2 — a different number.
    assert close(one(seeded, "SELECT avg(y) FROM st"), 5.2)


def test_the_argument_order_is_y_then_x(seeded):
    """`regr_slope(y, x)` is the slope of y ON x. Reversing the arguments
    returns the other regression line, and both are plausible numbers — which
    is exactly why this needs pinning rather than eyeballing."""
    assert close(one(seeded, "SELECT regr_slope(y, x) FROM st"), 2.1)
    assert close(one(seeded, "SELECT regr_slope(x, y) FROM st"), 0.46153846153846156)


# --- the degenerate cases ----------------------------------------------------- #


def test_empty_input(seeded):
    assert one(seeded, "SELECT regr_count(y, x) FROM st WHERE false") == 0
    for func in ("corr", "covar_pop", "covar_samp", "regr_avgx", "regr_sxx", "regr_slope"):
        assert one(seeded, f"SELECT {func}(y, x) FROM st WHERE false") is None, func


def test_a_single_pair(seeded):
    assert one(seeded, "SELECT regr_count(y, x) FROM st WHERE id = 1") == 1
    assert one(seeded, "SELECT regr_sxx(y, x) FROM st WHERE id = 1") == 0.0
    assert one(seeded, "SELECT covar_pop(y, x) FROM st WHERE id = 1") == 0.0
    # covar_samp needs two pairs; the correlations need variation.
    for func in ("covar_samp", "corr", "regr_slope", "regr_intercept", "regr_r2"):
        assert one(seeded, f"SELECT {func}(y, x) FROM st WHERE id = 1") is None, func


def test_rows_whose_x_is_null_contribute_nothing(seeded):
    assert one(seeded, "SELECT regr_count(y, x) FROM st WHERE id = 5") == 0
    assert one(seeded, "SELECT corr(y, x) FROM st WHERE id = 5") is None


def test_constant_x_and_constant_y_differ(conn):
    """The pair that guessing gets wrong: no variation in X leaves `regr_r2`
    NULL, no variation in Y makes it 1.0 — while `corr` is NULL for both."""
    conn.execute("CREATE TABLE cx (x float8, y float8)")
    conn.execute("INSERT INTO cx VALUES (1,2),(1,4)")  # constant x
    assert one(conn, "SELECT corr(y, x) FROM cx") is None
    assert one(conn, "SELECT regr_r2(y, x) FROM cx") is None
    assert one(conn, "SELECT regr_slope(y, x) FROM cx") is None
    assert one(conn, "SELECT covar_samp(y, x) FROM cx") == 0.0

    conn.execute("CREATE TABLE cy (x float8, y float8)")
    conn.execute("INSERT INTO cy VALUES (1,5),(2,5),(3,5)")  # constant y
    assert one(conn, "SELECT corr(y, x) FROM cy") is None
    assert one(conn, "SELECT regr_r2(y, x) FROM cy") == 1.0
    assert one(conn, "SELECT regr_slope(y, x) FROM cy") == 0.0
    assert one(conn, "SELECT regr_intercept(y, x) FROM cy") == 5.0


# --- in context --------------------------------------------------------------- #


def test_grouped(seeded):
    rows = seeded.execute("SELECT g, regr_count(y, x) FROM st GROUP BY g ORDER BY g").fetchall()
    assert rows == [("a", 3), ("b", 1)]


def test_grouped_correlation(seeded):
    rows = seeded.execute("SELECT g, corr(y, x) FROM st GROUP BY g ORDER BY g").fetchall()
    assert rows[0][0] == "a" and close(rows[0][1], 0.9933992677987828)
    assert rows[1] == ("b", None)  # one pair in group b: no variation


def test_having(seeded):
    assert close(one(seeded, "SELECT regr_slope(y, x) FROM st HAVING regr_count(y, x) > 2"), 2.1)


def test_result_types(seeded):
    """`regr_count` is int8; every other one is float8."""
    assert seeded.execute("SELECT regr_count(y, x) FROM st").description[0].type_code == 20
    for func in ("corr", "covar_pop", "regr_slope", "regr_avgx"):
        oid = seeded.execute(f"SELECT {func}(y, x) FROM st").description[0].type_code
        assert oid == 701, f"{func}: {oid}"


def test_integer_arguments_are_accepted(seeded):
    assert one(seeded, "SELECT corr(id, id) FROM st") == 1.0
