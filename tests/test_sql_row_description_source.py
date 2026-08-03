"""RowDescription reports each column's source table and attnum.

Postgres puts the source table's pg_class oid and the column's 1-based attnum
in every RowDescription field. We sent 0/0 for both, so a JDBC updatable
ResultSet could not resolve a result column back to its base column: its
``getBaseColumnName`` returns "" the moment the table oid is 0, and
``updateRow()`` then built ``UPDATE t SET "" = ?`` — which is what the server
rejected with ``column "" does not exist``.

Computed columns keep 0/0, which is what Postgres reports for them too.
"""

from __future__ import annotations

import pytest

from secantus.sql.engine import describe_statement, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage


@pytest.fixture()
def db(tmp_path):
    storage = Storage(str(tmp_path))
    session = Session(database="t")

    def run(sql: str):
        return run_sql(storage, "t", sql, session=session)[0]

    run("CREATE TABLE t (id int primary key, name text, dt date)")
    run("INSERT INTO t VALUES (1, 'a', DATE '2020-01-01')")
    run.storage = storage  # type: ignore[attr-defined]
    run.session = session  # type: ignore[attr-defined]
    try:
        yield run
    finally:
        storage.close()


def _table_oid(db) -> int:
    return db("SELECT oid FROM pg_catalog.pg_class WHERE relname = 't'").rows[0][0]


class TestExecutedSelect:
    def test_star_reports_every_column(self, db):
        oid = _table_oid(db)
        cols = db("SELECT * FROM t").columns
        assert [(c.name, c.table_oid, c.attnum) for c in cols] == [
            ("id", oid, 1),
            ("name", oid, 2),
            ("dt", oid, 3),
        ]

    def test_explicit_list_keeps_source_attnums(self, db):
        """The attnum is the column's position in the TABLE, not in the select
        list — reordering must not renumber it."""
        oid = _table_oid(db)
        cols = db("SELECT dt, id FROM t").columns
        assert [(c.name, c.table_oid, c.attnum) for c in cols] == [("dt", oid, 3), ("id", oid, 1)]

    def test_alias_keeps_the_source_identity(self, db):
        oid = _table_oid(db)
        (col,) = db("SELECT id AS renamed FROM t").columns
        assert (col.name, col.table_oid, col.attnum) == ("renamed", oid, 1)


class TestDescribePath:
    """The extended protocol describes without executing, and that is the path
    a JDBC client reads — it regressed independently of the executed path and
    needs its own cover."""

    def test_describe_reports_source_identity(self, db):
        import sqlglot

        from secantus.sql.catalog import Catalog

        oid = _table_oid(db)
        stmt = sqlglot.parse_one("SELECT id, name FROM t", read="postgres")
        cols = describe_statement(db.storage, "t", stmt, db.session, Catalog(db.storage))
        assert [(c.name, c.table_oid, c.attnum) for c in cols] == [
            ("id", oid, 1),
            ("name", oid, 2),
        ]


class TestComputedColumnsHaveNoSource:
    def test_expression_reports_zero(self, db):
        (col,) = db("SELECT 1 + 1 AS computed").columns
        assert (col.table_oid, col.attnum) == (0, 0)

    def test_aggregate_reports_zero(self, db):
        (col,) = db("SELECT count(*) FROM t").columns
        assert (col.table_oid, col.attnum) == (0, 0)


class TestWireEncoding:
    def test_row_description_carries_the_fields(self):
        from secantus.sql import pgwire

        payload = pgwire.row_description([("id", 23, -1, 16384, 2)])
        # "T" + int32 length + int16 field count, then the field: name NUL,
        # int32 table oid, int16 attnum.
        body = payload[5:]
        assert body[2:5] == b"id\x00"
        assert int.from_bytes(body[5:9], "big") == 16384
        assert int.from_bytes(body[9:11], "big") == 2

    def test_defaults_stay_zero_when_omitted(self):
        from secantus.sql import pgwire

        body = pgwire.row_description([("x", 23)])[5:]
        assert int.from_bytes(body[4:8], "big") == 0
        assert int.from_bytes(body[8:10], "big") == 0
