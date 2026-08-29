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


# ---------------------------------------------------------------------------
# Partial indexes. `indexdef` used to drop the WHERE clause entirely, so the
# rendered statement claimed a FULL index — a tool recreating an index from
# `pg_indexes` built the wrong one. The predicate is now reversed back to SQL.
#
# Every expectation below is PostgreSQL 14's own rendering, byte for byte,
# including the parenthesisation and the `::text` cast on string literals.
# ---------------------------------------------------------------------------


def _indexdef(storage, name):
    rows = _rows(storage, f"SELECT indexdef FROM pg_indexes WHERE indexname = '{name}'")
    return rows[0][0] if rows else None


@pytest.mark.parametrize(
    "predicate,rendered",
    [
        ("b > 5", "(b > 5)"),
        ("b >= 5", "(b >= 5)"),
        ("b < 5", "(b < 5)"),
        ("b <= 5", "(b <= 5)"),
        ("b = 5", "(b = 5)"),
        ("b IS NOT NULL", "(b IS NOT NULL)"),
        ("b > 5 AND a < 2", "((b > 5) AND (a < 2))"),
        ("b > 5 OR a < 2", "((b > 5) OR (a < 2))"),
    ],
)
def test_partial_predicate_round_trips(storage, predicate, rendered):
    run_sql(
        storage, DB, f"CREATE INDEX pix ON t (a) WHERE {predicate}", session=Session(database=DB)
    )
    assert (
        _indexdef(storage, "pix")
        == f"CREATE INDEX pix ON public.t USING btree (a) WHERE {rendered}"
    )


def test_not_equal_round_trips_through_its_desugaring(storage):
    """`b <> 5` is stored as an $and of "not equal" AND "not null" — the
    lowering, not the user's predicate. PostgreSQL renders the original, so the
    idiom is recognised rather than leaked."""
    run_sql(storage, DB, "CREATE INDEX pix ON t (a) WHERE b <> 5", session=Session(database=DB))
    assert _indexdef(storage, "pix").endswith(" WHERE (b <> 5)")


def test_string_literals_carry_the_cast(storage):
    """PostgreSQL prints `(s = 'x'::text)`, not `(s = 'x')`."""
    q = Session(database=DB)
    run_sql(storage, DB, "ALTER TABLE t ADD COLUMN s text", session=q)
    run_sql(storage, DB, "CREATE INDEX pix ON t (a) WHERE s = 'x'", session=q)
    assert _indexdef(storage, "pix").endswith(" WHERE (s = 'x'::text)")


def test_a_quote_in_a_literal_is_escaped(storage):
    q = Session(database=DB)
    run_sql(storage, DB, "ALTER TABLE t ADD COLUMN s text", session=q)
    run_sql(storage, DB, "CREATE INDEX pix ON t (a) WHERE s = 'O''Brien'", session=q)
    assert _indexdef(storage, "pix").endswith(" WHERE (s = 'O''Brien'::text)")


def test_unique_and_multi_column_partial_indexes(storage):
    q = Session(database=DB)
    run_sql(storage, DB, "CREATE UNIQUE INDEX upix ON t (a) WHERE b < 9", session=q)
    run_sql(storage, DB, "CREATE INDEX mpix ON t (a, b) WHERE b >= 1", session=q)
    assert _indexdef(storage, "upix") == (
        "CREATE UNIQUE INDEX upix ON public.t USING btree (a) WHERE (b < 9)"
    )
    assert _indexdef(storage, "mpix") == (
        "CREATE INDEX mpix ON public.t USING btree (a, b) WHERE (b >= 1)"
    )


def test_a_non_partial_index_gains_no_where_clause(storage):
    """The guard: only partial indexes get a predicate."""
    assert _indexdef(storage, "idx_a") == "CREATE INDEX idx_a ON public.t USING btree (a)"
    assert " WHERE " not in _indexdef(storage, "uq_a")
