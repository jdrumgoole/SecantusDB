"""Index / constraint reflection for ``\\d`` (#134): ``pg_catalog.pg_indexes``
with rendered ``indexdef`` (including ``DESC`` columns and ``UNIQUE``),
``pg_get_indexdef(oid)``, and ``pg_get_constraintdef(oid)`` rendering PRIMARY
KEY / FOREIGN KEY / UNIQUE / CHECK the way psql and SQLAlchemy read them.
Driven over the real ``Storage``.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "app"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    sess = Session(database=DB)

    def q(sql):
        run_sql(s, DB, sql, session=sess)

    q("CREATE TABLE parent (id int PRIMARY KEY)")
    q("CREATE TABLE t (id int PRIMARY KEY, a int, b int, p int)")
    q("ALTER TABLE t ADD CONSTRAINT t_p_fkey FOREIGN KEY (p) REFERENCES parent(id)")
    q("ALTER TABLE t ADD CONSTRAINT t_b_key UNIQUE (b)")
    q("ALTER TABLE t ADD CONSTRAINT t_check CHECK (id > 0)")
    q("CREATE INDEX idx_a ON t (a)")
    q("CREATE INDEX idx_ab ON t (a, b)")
    q("CREATE INDEX idx_desc ON t (a DESC, b)")
    q("CREATE UNIQUE INDEX uq_a ON t (a)")
    try:
        yield s
    finally:
        s.close()


def _rows(storage, sql):
    return run_sql(storage, DB, sql, session=Session(database=DB))[-1].rows


def test_pg_indexes_rows(storage):
    rows = _rows(
        storage,
        "SELECT indexname, indexdef FROM pg_catalog.pg_indexes "
        "WHERE tablename = 't' ORDER BY indexname",
    )
    assert rows == [
        ("idx_a", "CREATE INDEX idx_a ON public.t USING btree (a)"),
        ("idx_ab", "CREATE INDEX idx_ab ON public.t USING btree (a, b)"),
        ("idx_desc", "CREATE INDEX idx_desc ON public.t USING btree (a DESC, b)"),
        ("t_b_key", "CREATE UNIQUE INDEX t_b_key ON public.t USING btree (b)"),
        ("t_pkey", "CREATE UNIQUE INDEX t_pkey ON public.t USING btree (id)"),
        ("uq_a", "CREATE UNIQUE INDEX uq_a ON public.t USING btree (a)"),
    ]


def test_pg_indexes_schema_and_tablespace(storage):
    rows = _rows(
        storage,
        "SELECT schemaname, tablespace FROM pg_catalog.pg_indexes WHERE indexname = 'idx_a'",
    )
    assert rows == [("public", None)]


def test_no_wiredtiger_id_index_leaks(storage):
    # WiredTiger's physical ``_id_`` index must never surface as a SQL index —
    # the primary key is reflected as ``<table>_pkey``.
    rows = _rows(storage, "SELECT indexname FROM pg_catalog.pg_indexes")
    names = {r[0] for r in rows}
    assert "_id_" not in names
    assert "t_pkey" in names
    assert "parent_pkey" in names


def test_pg_get_indexdef_by_oid(storage):
    rows = _rows(
        storage,
        "SELECT pg_get_indexdef(c.oid) FROM pg_catalog.pg_class c WHERE c.relname = 'idx_desc'",
    )
    assert rows == [("CREATE INDEX idx_desc ON public.t USING btree (a DESC, b)",)]


def test_pg_get_indexdef_unknown_oid(storage):
    assert _rows(storage, "SELECT pg_get_indexdef(999999)") == [(None,)]


def test_pg_get_constraintdef_all_types(storage):
    rows = _rows(
        storage,
        "SELECT conname, pg_get_constraintdef(oid) FROM pg_catalog.pg_constraint ORDER BY conname",
    )
    by_name = dict(rows)
    assert by_name["parent_pkey"] == "PRIMARY KEY (id)"
    assert by_name["t_pkey"] == "PRIMARY KEY (id)"
    assert by_name["t_p_fkey"] == "FOREIGN KEY (p) REFERENCES parent(id)"
    assert by_name["t_b_key"] == "UNIQUE (b)"
    assert by_name["t_check"] == "CHECK ((id > 0))"


def test_composite_pk_constraintdef(tmp_path):
    s = Storage(str(tmp_path))
    sess = Session(database=DB)
    run_sql(s, DB, "CREATE TABLE ct (a int, b int, PRIMARY KEY (a, b))", session=sess)
    try:
        rows = run_sql(
            s,
            DB,
            "SELECT pg_get_constraintdef(oid) FROM pg_catalog.pg_constraint "
            "WHERE conname = 'ct_pkey'",
            session=sess,
        )[-1].rows
        assert rows == [("PRIMARY KEY (a, b)",)]
    finally:
        s.close()
