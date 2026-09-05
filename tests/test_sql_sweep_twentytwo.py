"""A twenty-second sweep — a record rendered as JSON, and half of
`strip_nulls`.

Two defects with the same shape as several found earlier in this campaign: a
value that is a **dict internally** reaching a renderer that does not know what
it is, and a function whose two spellings take different code paths.

**`record::text` produced JSON.** `('a', 1)::text` answered
`{"f1": "a", "f2": 1}` where PostgreSQL gives `(a,1)`. The record renderer
already existed and is what the WIRE uses for a composite column — only the
CAST did not route to it. Same fix, same place, as the `tsvector::text` bug two
batches earlier; the branch now recognises three internal-dict types before
falling through to JSON.

**`json_strip_nulls` answered NULL for every input**, while `jsonb_strip_nulls`
worked. sqlglot gives the non-`b` spelling its own node and leaves the `b`
spelling an anonymous call, so the name-keyed dispatch served one and the other
fell through to NULL. That is the third time in this campaign a dedicated
sqlglot node has silently bypassed a name-keyed handler (`to_number`,
`ToNumber`; `json_strip_nulls`, `JSONStripNulls`) — worth checking for whenever
a function "exists" but answers NULL.

Note what `strip_nulls` does NOT do: array elements are left alone.
`json_strip_nulls('[1,null,2]')` is `[1,null,2]`, because only a KEY can be
absent.

Every expectation here was measured against PostgreSQL 14.13.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")


@pytest.fixture
def conn(tmp_path):
    st = Storage(str(tmp_path / "s22"))
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


# --- a record renders as a record, not as JSON ------------------------------- #


@pytest.mark.parametrize(
    "sql,expected",
    [
        ("SELECT ('a'::text, 1)::text", "(a,1)"),
        ("SELECT ROW('a'::text, 1)::text", "(a,1)"),
        ("SELECT (1, 2, 3)::text", "(1,2,3)"),
        ("SELECT ROW(1)::text", "(1)"),
        # A field needing quotes, and the doubled quote inside it.
        ("SELECT ('a,b'::text, 'c\"d'::text)::text", '("a,b","c""d")'),
        # A NULL field is empty; an empty string is quoted, which is the only
        # thing distinguishing the two in the record's text form.
        ("SELECT (NULL::int, 1)::text", "(,1)"),
        ("SELECT ('', 'x')::text", '("",x)'),
        # A char(n) field keeps its blank padding here (see sweep thirteen).
        ("SELECT ('a'::text, 'd'::char(2))::text", '(a,"d ")'),
    ],
)
def test_record_to_text(conn, sql, expected):
    assert one(conn, sql) == expected


# --- json_strip_nulls -------------------------------------------------------- #


def test_json_strip_nulls_removes_null_keys(conn):
    assert one(conn, """SELECT json_strip_nulls('{"a":null,"b":1}'::json)::text""") == '{"b": 1}'


def test_json_strip_nulls_recurses(conn):
    assert (
        one(conn, """SELECT json_strip_nulls('{"a":{"b":null,"c":2}}'::json)::text""")
        == '{"a": {"c": 2}}'
    )


def test_json_strip_nulls_leaves_array_elements(conn):
    """Only a KEY can be absent, so a null array element stays."""
    assert one(conn, """SELECT json_strip_nulls('[1,null,2]'::json)::text""") == "[1, null, 2]"


def test_json_strip_nulls_of_null(conn):
    assert one(conn, "SELECT json_strip_nulls(NULL)") is None


def test_both_spellings_agree(conn):
    """The bug was that they did not: one worked and one answered NULL."""
    plain = one(conn, """SELECT json_strip_nulls('{"a":null,"b":1}'::json)::text""")
    binary = one(conn, """SELECT jsonb_strip_nulls('{"a":null,"b":1}'::jsonb)::text""")
    assert plain == binary == '{"b": 1}'


def test_strip_nulls_over_a_column(conn):
    conn.execute("CREATE TABLE jr (id int, jb jsonb)")
    conn.execute("""INSERT INTO jr VALUES (1, '{"a":null,"b":1}')""")
    assert one(conn, "SELECT jsonb_strip_nulls(jb)::text FROM jr") == '{"b": 1}'
