"""``tsrange`` / ``tstzrange`` bounds keep their microseconds through storage.

A range is stored as a subdocument and its bounds are datetimes, which BSON
holds to the MILLISECOND. So a stored ``[…49.338943, …)`` read back as
``[…49.338000, …)``, silently. Scalar ``timestamp`` columns were fixed with a
hidden companion field (`secantus.sql.subms`); range bounds never were, and the
backlog entry that named them was closed after measuring scalars only. psycopg's
randomised ``test_adapt::test_random`` / ``test_copy`` suites caught it whenever
a draw's microseconds did not land on a whole millisecond.

Postgres' ``timestamp`` is microsecond-exact, so every expectation here is the
round-trip identity or a comparison Postgres defines on exact values.

Also pinned: two pre-existing range bugs found alongside it --
``stored && tsrange(…)`` was an XX000 (a stored bound decodes naive, a
constructed one is aware, and Python will not order the two), and
``WHERE r = '<literal>'`` compared whole BSON subdocuments, so it missed a row
holding exactly the literal's value.
"""

from __future__ import annotations

import datetime as dt

import pytest

import pg_oracle

psycopg = pytest.importorskip("psycopg")

from psycopg.types.multirange import Multirange  # noqa: E402
from psycopg.types.range import Range  # noqa: E402

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

# Both bounds inside ONE millisecond: any truncation collapses the range to
# `[.123000,.123000)`, i.e. empty -- so a lost remainder cannot hide.
A = dt.datetime(2024, 5, 1, 10, 0, 0, 123456)
B = dt.datetime(2024, 5, 1, 10, 0, 0, 123999)
R = Range(A, B, "[)")
LIT = "'[2024-05-01 10:00:00.123456,2024-05-01 10:00:00.123999)'"


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        c = psycopg.connect(
            host=host, port=port, user="postgres", dbname="postgres", autocommit=True
        )
        try:
            c.execute(
                "create table t (id int primary key, r tsrange, z tstzrange,"
                " m tsmultirange, arr tsrange[])"
            )
            yield c
        finally:
            c.close()
    finally:
        srv.stop()
        st.close()


def _r(conn, row_id: int):
    return conn.execute("select r from t where id = %s", (row_id,)).fetchone()[0]


def test_insert_literal(conn) -> None:
    conn.execute(f"insert into t (id, r) values (1, {LIT})")
    assert _r(conn, 1) == R


def test_insert_bound_parameter(conn) -> None:
    conn.execute("insert into t (id, r) values (1, %s)", (R,))
    assert _r(conn, 1) == R


def test_tstzrange(conn) -> None:
    utc = dt.timezone.utc
    z = Range(A.replace(tzinfo=utc), B.replace(tzinfo=utc), "[)")
    conn.execute("insert into t (id, z) values (1, %s)", (z,))
    assert conn.execute("select z from t where id = 1").fetchone()[0] == z


def test_insert_select(conn) -> None:
    conn.execute("insert into t (id, r) values (1, %s)", (R,))
    conn.execute("insert into t (id, r) select 2, r from t where id = 1")
    assert _r(conn, 2) == R


def test_update_literal_and_expression(conn) -> None:
    conn.execute("insert into t (id, r) values (1, '[2020-01-01,2020-01-02)')")
    conn.execute(f"update t set r = {LIT} where id = 1")
    assert _r(conn, 1) == R

    conn.execute("insert into t (id, r) values (2, '[2024-05-01,2024-05-02)')")
    conn.execute(
        "update t set r = r * tsrange('2024-05-01 10:00:00.123456',"
        " '2024-05-01 10:00:00.123999') where id = 2"
    )
    assert _r(conn, 2) == R


def test_update_clears_a_stale_remainder(conn) -> None:
    """A whole-millisecond value written over a sub-ms one must not inherit the
    old remainder -- the companion invariant, for the in-subdocument form."""
    conn.execute("insert into t (id, r) values (1, %s)", (R,))
    whole = Range(dt.datetime(2024, 5, 1, 10), dt.datetime(2024, 5, 1, 11), "[)")
    conn.execute("update t set r = %s where id = 1", (whole,))
    assert _r(conn, 1) == whole


@pytest.mark.parametrize("fmt", ["text", "binary"])
def test_copy_from(conn, fmt: str) -> None:
    sql = "copy t (id, r) from stdin" + (" (format binary)" if fmt == "binary" else "")
    with conn.cursor().copy(sql) as cp:
        if fmt == "binary":
            cp.set_types(["int4", "tsrange"])
        cp.write_row((1, R))
    assert _r(conn, 1) == R


def test_multirange_and_array(conn) -> None:
    conn.execute("insert into t (id, m, arr) values (1, %s, %s)", (Multirange([R]), [R, R]))
    m, arr = conn.execute("select m, arr from t where id = 1").fetchone()
    assert m == Multirange([R])
    assert arr == [R, R]


def test_text_rendering_and_bounds(conn) -> None:
    conn.execute("insert into t (id, r) values (1, %s)", (R,))
    text, lo, hi = conn.execute("select r::text, lower(r), upper(r) from t").fetchone()
    assert text == '["2024-05-01 10:00:00.123456","2024-05-01 10:00:00.123999")'
    # `lower()` / `upper()` of a tsrange are typed timestamptz here, which
    # Postgres types timestamp (tasks/backlog.md) -- compare the instant only.
    assert lo.replace(tzinfo=None) == A
    assert hi.replace(tzinfo=None) == B


def test_containment_within_one_millisecond(conn) -> None:
    conn.execute("insert into t (id, r) values (1, %s)", (R,))
    inside, outside = conn.execute(
        "select r @> '2024-05-01 10:00:00.1235'::timestamp,"
        " r @> '2024-05-01 10:00:00.1234'::timestamp from t"
    ).fetchone()
    assert (inside, outside) == (True, False)


def test_equality_compares_values_not_representations(conn) -> None:
    conn.execute("insert into t (id, r) values (1, %s)", (R,))
    conn.execute("insert into t (id, r) values (2, '[2020-01-01,2020-01-02)')")
    assert conn.execute(f"select id from t where r = {LIT}").fetchall() == [(1,)]
    # Differs only in the microseconds: not equal.
    near = "'[2024-05-01 10:00:00.123457,2024-05-01 10:00:00.123999)'"
    assert conn.execute(f"select id from t where r = {near}").fetchall() == []
    assert conn.execute(f"select id from t where r <> {LIT} order by id").fetchall() == [(2,)]


def test_stored_range_against_a_constructed_one(conn) -> None:
    """Was XX000: the stored bound decodes naive, `tsrange(…)`'s is aware."""
    conn.execute("insert into t (id, r) values (1, '[2020-01-01,2020-01-05)')")
    overlaps, inter = conn.execute(
        "select r && tsrange('2020-01-02', '2020-01-03'),"
        " r * tsrange('2020-01-02', '2020-01-03') from t"
    ).fetchone()
    assert overlaps is True
    assert inter == Range(dt.datetime(2020, 1, 2), dt.datetime(2020, 1, 3), "[)")


# The same statements against the Python server and a real PostgreSQL.
# `import pg_oracle` above is what puts this file in CI's `pg-oracle` lane.
_ORACLE_SCRIPT = [
    "drop table if exists rsub",
    "create table rsub (id int primary key, r tsrange, z tstzrange)",
    f"insert into rsub values (1, {LIT}, "
    "'[2024-05-01 10:00:00.123456+00,2024-05-01 10:00:00.123999+00)')",
    "insert into rsub select 2, r, z from rsub where id = 1",
    "insert into rsub values (3, '[2020-01-01,2020-01-05)', null)",
    f"update rsub set r = r * {LIT}::tsrange where id = 2",
]
_ORACLE_QUERIES = [
    "select id, r::text from rsub order by id",
    # Not `lower(r)::text`: `lower()` of a tsrange is typed timestamptz here
    # (Postgres: timestamp), so its text grows a `+00` -- a separate, pre-existing
    # divergence tracked in tasks/backlog.md, not this file's subject.
    "select id from rsub where r @> '2024-05-01 10:00:00.1235'::timestamp order by id",
    "select id from rsub where r @> '2024-05-01 10:00:00.1234'::timestamp order by id",
    f"select id from rsub where r = {LIT} order by id",
    "select id from rsub where r = "
    "'[2024-05-01 10:00:00.123457,2024-05-01 10:00:00.123999)' order by id",
    "select id, r && tsrange('2020-01-02', '2020-01-03') from rsub order by id",
    "select id, (r * tsrange('2020-01-02', '2020-01-03'))::text from rsub order by id",
    "select z = '[2024-05-01 10:00:00.123456+00,2024-05-01 10:00:00.123999+00)' "
    "from rsub where id = 1",
]


def _answers(c) -> list:
    c.execute("set timezone = 'UTC'")
    for stmt in _ORACLE_SCRIPT:
        c.execute(stmt)
    return [c.execute(q).fetchall() for q in _ORACLE_QUERIES]


@pytest.mark.skipif(not pg_oracle.available(), reason=pg_oracle.skip_reason())
def test_matches_real_postgres(conn) -> None:
    pg = pg_oracle.connect()
    assert pg is not None
    try:
        theirs = _answers(pg)
        # Self-check: the scenario must exercise sub-millisecond bounds on the
        # REFERENCE server, or every comparison below is vacuous.
        assert theirs[0][0][1] == '["2024-05-01 10:00:00.123456","2024-05-01 10:00:00.123999")'
        pg.execute("drop table rsub")
    finally:
        pg.close()
    ours = _answers(conn)
    for q, t, o in zip(_ORACLE_QUERIES, theirs, ours, strict=True):
        assert o == t, f"{q}\n  postgres: {t}\n  secantus: {o}"
