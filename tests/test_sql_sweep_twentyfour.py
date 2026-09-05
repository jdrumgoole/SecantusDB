"""A twenty-fourth sweep — whole-row references.

PostgreSQL lets a table or sub-select ALIAS stand for the whole row:
`row_to_json(t) FROM (SELECT ...) t` is one of the most common ways to get a
row out as JSON. Every form of it answered `42703 column "t" does not exist`.

The blocker was structural rather than a missing branch. `Resolve` maps a node
to ONE document field path, and a whole-row reference is not a path — it is a
synthetic composite of every column in the relation. Two pieces were needed:

* the evaluated scope builds the record when it sees a bare relation name,
  instead of handing it to the column resolver, and
* `SELECT t FROM t` — where the reference is the projection itself — has to
  ROUTE to the evaluated path, because a plain `$project` of columns cannot
  produce a composite at all.

**A real column of the same name wins**, which is what PostgreSQL does. Routing
on the syntactic match alone is still correct for that case: the resolver
prefers the column, so a table with a column named after itself only takes the
slower path.

One gap remains and is deliberate: `SELECT t FROM t` reports the generic
`RECORD` oid (2249) where PostgreSQL reports the table's own rowtype oid.
That is a type identity, not a wrong answer — the field values are right — and
minting per-table rowtype oids is a catalog feature. It replaces a hard error,
so it ships.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s24"))
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
    conn.execute("CREATE TABLE jr (id int PRIMARY KEY, a int, b text)")
    conn.execute("INSERT INTO jr VALUES (1,10,'x'),(2,20,'y')")
    return conn


def one(c, sql):
    return c.execute(sql).fetchone()[0]


# --- the idiom this was really about ----------------------------------------- #


def test_row_to_json_over_a_derived_table(conn):
    assert one(conn, "SELECT row_to_json(t) FROM (SELECT 1 AS a, 'b' AS b) t") == {
        "a": 1,
        "b": "b",
    }


def test_row_to_json_over_a_derived_table_from_a_real_one(seeded):
    assert one(seeded, "SELECT row_to_json(t) FROM (SELECT id, a FROM jr WHERE id=1) t") == {
        "id": 1,
        "a": 10,
    }


def test_to_json_over_a_derived_table(conn):
    assert one(conn, "SELECT to_json(r) FROM (SELECT 1 AS a) r") == {"a": 1}


def test_row_to_json_over_a_base_table(seeded):
    assert seeded.execute("SELECT row_to_json(jr) FROM jr ORDER BY id").fetchall() == [
        ({"id": 1, "a": 10, "b": "x"},),
        ({"id": 2, "a": 20, "b": "y"},),
    ]


def test_whole_row_cast_to_text(seeded):
    assert one(seeded, "SELECT (jr)::text FROM jr WHERE id=1") == "(1,10,x)"


# --- the projection form ------------------------------------------------------ #


def test_bare_whole_row_projection(seeded):
    """`SELECT t FROM t` had to route to the evaluated path: a plain projection
    of columns cannot build a composite."""
    row = seeded.execute("SELECT jr FROM jr WHERE id=1").fetchone()[0]
    # Reported as generic RECORD rather than the table's rowtype, so psycopg
    # hands back the parsed fields; the VALUES are what matter here.
    assert tuple(str(v) for v in row) == ("1", "10", "x")


# --- a column named after its table still wins -------------------------------- #


def test_a_real_column_shadows_the_whole_row(conn):
    conn.execute("CREATE TABLE t (t int, other int)")
    conn.execute("INSERT INTO t VALUES (7, 8)")
    assert one(conn, "SELECT t FROM t") == 7
    assert one(conn, "SELECT t.t FROM t") == 7


# --- nothing else changed ----------------------------------------------------- #


def test_ordinary_projections_are_unaffected(seeded):
    assert seeded.execute("SELECT id, a FROM jr ORDER BY id").fetchall() == [(1, 10), (2, 20)]
    assert one(seeded, "SELECT b FROM jr WHERE id=2") == "y"


def test_qualified_columns_are_unaffected(seeded):
    assert seeded.execute("SELECT jr.id, jr.b FROM jr ORDER BY jr.id").fetchall() == [
        (1, "x"),
        (2, "y"),
    ]


def test_star_is_unaffected(seeded):
    assert seeded.execute("SELECT * FROM jr ORDER BY id").fetchall() == [
        (1, 10, "x"),
        (2, 20, "y"),
    ]
