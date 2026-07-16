"""EXPLAIN for the SQL layer (#122): the QUERY PLAN result shape, the faithful
IXSCAN/COLLSCAN scan node (Seq Scan vs Index Scan), option parsing (ANALYZE /
FORMAT JSON / VERBOSE), and plan nodes for SELECT / UPDATE / DELETE / INSERT and
pipeline queries. (End-to-end wire coverage is in test_pgserver_pg8000.py.)
"""

from __future__ import annotations

import json

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"

_STORAGES: list = []


def _new_storage():
    import tempfile

    d = tempfile.mkdtemp()
    st = Storage(d)
    _STORAGES.append((st, d))
    return st


@pytest.fixture(autouse=True)
def _close_storages():
    import shutil

    yield
    while _STORAGES:
        st, d = _STORAGES.pop()
        st.close()
        shutil.rmtree(d, ignore_errors=True)


def _fresh(with_index=False):
    st = _new_storage()
    sess = Session(database=DB)
    run_sql(st, DB, "CREATE TABLE t (id int, name text)", session=sess)
    run_sql(st, DB, "INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c')", session=sess)
    if with_index:
        run_sql(st, DB, "CREATE INDEX t_id_idx ON t (id)", session=sess)
    return st, sess


def _run(st, sess, sql):
    return run_sql(st, DB, sql, session=sess)[-1]


def _plan(st, sess, sql):
    return "\n".join(r[0] for r in _run(st, sess, sql).rows)


# --------------------------------------------------------------------------- #
# Result shape
# --------------------------------------------------------------------------- #


def test_explain_result_shape():
    st, sess = _fresh()
    r = _run(st, sess, "EXPLAIN SELECT * FROM t")
    assert r.command_tag == "EXPLAIN"
    assert [c.name for c in r.columns] == ["QUERY PLAN"]
    assert r.rows  # at least one plan line


# --------------------------------------------------------------------------- #
# Scan nodes: COLLSCAN vs IXSCAN
# --------------------------------------------------------------------------- #


def test_seq_scan_without_index():
    st, sess = _fresh(with_index=False)
    plan = _plan(st, sess, "EXPLAIN SELECT * FROM t WHERE id = 1")
    assert "Seq Scan on t" in plan
    assert "Filter: (id = 1)" in plan
    assert "Index Scan" not in plan


def test_index_scan_with_index():
    st, sess = _fresh(with_index=True)
    plan = _plan(st, sess, "EXPLAIN SELECT * FROM t WHERE id = 1")
    assert "Index Scan using t_id_idx on t" in plan
    assert "Index Cond: (id = 1)" in plan
    assert "Seq Scan" not in plan


def test_no_where_no_filter_line():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN SELECT * FROM t")
    assert "Seq Scan on t" in plan
    assert "Filter:" not in plan


def test_order_by_uses_index():
    st, sess = _fresh(with_index=True)
    plan = _plan(st, sess, "EXPLAIN SELECT * FROM t ORDER BY id")
    assert "Index Scan using t_id_idx on t" in plan


# --------------------------------------------------------------------------- #
# ANALYZE
# --------------------------------------------------------------------------- #


def test_analyze_reports_actual_rows():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN ANALYZE SELECT * FROM t WHERE id = 2")
    assert "actual rows=1 loops=1" in plan


def test_analyze_executes_writes():
    # EXPLAIN ANALYZE on a DML actually performs the modification (as Postgres does).
    st, sess = _fresh()
    _run(st, sess, "EXPLAIN ANALYZE INSERT INTO t VALUES (4,'d')")
    assert _run(st, sess, "SELECT count(*) FROM t").rows == [(4,)]


def test_plain_explain_does_not_execute_writes():
    st, sess = _fresh()
    _run(st, sess, "EXPLAIN INSERT INTO t VALUES (99,'x')")
    assert _run(st, sess, "SELECT count(*) FROM t").rows == [(3,)]


# --------------------------------------------------------------------------- #
# FORMAT JSON
# --------------------------------------------------------------------------- #


def test_format_json_structure():
    st, sess = _fresh(with_index=True)
    r = _run(st, sess, "EXPLAIN (FORMAT JSON) SELECT * FROM t WHERE id = 1")
    assert len(r.rows) == 1
    doc = json.loads(r.rows[0][0])
    plan = doc[0]["Plan"]
    assert plan["Node Type"] == "Index Scan"
    assert plan["Index Name"] == "t_id_idx"
    assert plan["Relation Name"] == "t"
    assert plan["Index Cond"] == "(id = 1)"


def test_format_json_nested_plans():
    st, sess = _fresh()
    r = _run(st, sess, "EXPLAIN (FORMAT JSON) SELECT name, count(*) FROM t GROUP BY name")
    plan = json.loads(r.rows[0][0])[0]["Plan"]
    assert plan["Node Type"] == "GroupAggregate"
    assert plan["Plans"][0]["Node Type"] == "Seq Scan"


def test_unsupported_format_rejected():
    st, sess = _fresh()
    with pytest.raises(errors.SQLError) as exc:
        _run(st, sess, "EXPLAIN (FORMAT YAML) SELECT 1")
    assert exc.value.sqlstate == "0A000"


# --------------------------------------------------------------------------- #
# DML + pipeline plan nodes
# --------------------------------------------------------------------------- #


def test_update_node():
    st, sess = _fresh(with_index=True)
    plan = _plan(st, sess, "EXPLAIN UPDATE t SET name='z' WHERE id = 1")
    assert "Update on t" in plan
    assert "->  Index Scan using t_id_idx on t" in plan


def test_delete_node():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN DELETE FROM t WHERE id = 1")
    assert "Delete on t" in plan
    assert "->  Seq Scan on t" in plan


def test_insert_node():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN INSERT INTO t VALUES (4,'d')")
    assert "Insert on t" in plan
    assert "->  Result" in plan


def test_group_aggregate_node():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN SELECT name, count(*) FROM t GROUP BY name")
    assert plan.startswith("GroupAggregate")
    assert "->  Seq Scan on t" in plan


def test_bare_aggregate_node():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN SELECT sum(id) FROM t")
    assert plan.startswith("Aggregate")


def test_constant_select_result_node():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN SELECT 1")
    assert plan.strip().startswith("Result")


# --------------------------------------------------------------------------- #
# Option-word forms
# --------------------------------------------------------------------------- #


def test_bare_analyze_verbose_words():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN ANALYZE VERBOSE SELECT * FROM t WHERE id = 1")
    assert "actual rows=1" in plan
    assert "Seq Scan on t" in plan


def test_paren_analyze_verbose():
    st, sess = _fresh()
    plan = _plan(st, sess, "EXPLAIN (ANALYZE, VERBOSE, COSTS OFF) SELECT * FROM t WHERE id = 1")
    assert "actual rows=1" in plan
