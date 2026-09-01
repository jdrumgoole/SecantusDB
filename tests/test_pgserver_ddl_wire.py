"""Wire-level replies for the statements that carry a ``SELECT n`` tag but
send no rows to the client.

``CREATE TABLE AS`` / ``SELECT INTO`` / ``CREATE MATERIALIZED VIEW`` report the
number of rows they WROTE, which drivers surface as `cursor.rowcount`. The wire
layer decided whether to send a RowDescription from the command tag alone, so
all three sent an EMPTY one — and psycopg then read the result as a zero-row
result set, reporting `rowcount` 0 instead of the rows written and
`description` `[]` instead of `None`.
"""

from __future__ import annotations

import pytest

psycopg = pytest.importorskip("psycopg")

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402


@pytest.fixture()
def server(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        st.close()


def _connect(srv):
    host, port = srv.address
    return psycopg.connect(host=host, port=port, dbname="db", user="joe", autocommit=True)


@pytest.mark.parametrize(
    ("sql", "expected_rows"),
    [
        ("CREATE TABLE ct AS SELECT x FROM b", 2),
        ("SELECT x INTO si FROM b", 2),
        ("CREATE MATERIALIZED VIEW mv AS SELECT x FROM b", 2),
    ],
)
def test_row_writing_ddl_reports_its_rowcount_and_no_description(server, sql, expected_rows):
    with _connect(server) as conn:
        c = conn.cursor()
        c.execute("CREATE TABLE b (x int)")
        c.execute("INSERT INTO b VALUES (1), (2)")
        c.execute(sql)
        assert c.statusmessage == f"SELECT {expected_rows}"
        assert c.description is None
        assert c.rowcount == expected_rows


def test_a_plain_select_still_describes_its_columns(server):
    """The control: the fix must not stop a real SELECT sending a
    RowDescription."""
    with _connect(server) as conn:
        c = conn.cursor()
        c.execute("CREATE TABLE b (x int)")
        c.execute("INSERT INTO b VALUES (1), (2)")
        c.execute("SELECT x FROM b ORDER BY x")
        assert c.description is not None
        assert c.fetchall() == [(1,), (2,)]
        c.execute("SELECT x FROM b WHERE x > 99")
        assert c.description is not None
        assert c.fetchall() == []
