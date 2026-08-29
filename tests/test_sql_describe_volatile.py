"""Describe must not execute volatile functions; regclass resolution;
DateStyle slash-date input; array_fill; geometric binary results.

The pgjdbc clusters behind these: StatementTest's setQueryTimeout trio
(Describe of ``select pg_sleep(n)`` slept, swallowed the cancel into NoData,
then Execute's DataRow crashed the driver), SearchPathLookupTest (regclass of
qualified names), ResultSetTest.testTimestamp (MDY slash dates + array_fill),
and GeometricTest's binary-mode PGpoint/PGbox round-trips.
"""

from __future__ import annotations

import struct
import time

import pytest

from secantus.sql import errors, run_sql, typemap
from secantus.sql.pgextended import ExtendedSession
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"


@pytest.fixture(autouse=True)
def _reset_render_session():
    # Tests below bind the render session for DateStyle-aware parsing; the
    # contextvar must not leak into other tests sharing the xdist worker
    # (a leaked session made tsrange renders grow +00:00 offsets on CI).
    yield
    typemap.set_render_session(None)


@pytest.fixture
def st(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def one(st, sql, sess=None):
    return run_sql(st, DB, sql, session=sess or Session(database=DB))[-1].rows[0][0]


class TestDescribeVolatile:
    def _describe(self, ext, query):
        ext.process("P", b"\x00" + query.encode() + b"\x00" + struct.pack(">h", 0))
        ext.process("B", b"\x00\x00" + struct.pack(">hhh", 0, 0, 0))
        return bytes(ext.process("D", b"P\x00"))

    def test_pg_sleep_describe_is_instant_and_typed(self, st):
        ext = ExtendedSession(st, Session(database=DB))
        t0 = time.monotonic()
        reply = self._describe(ext, "select pg_sleep(5)")
        assert time.monotonic() - t0 < 1.0, "Describe executed pg_sleep"
        assert reply[:1] == b"T", "Describe must answer RowDescription, not NoData"

    def test_pg_sleep_returns_void(self, st):
        # PG types pg_sleep as void (oid 2278) with a NULL value, not text.
        res = run_sql(st, DB, "select pg_sleep(0)", session=Session(database=DB))[-1]
        assert res.columns[0].pg_oid == 2278
        assert res.columns[0].type_tag == "void"
        assert res.rows[0][0] is None

    def test_nextval_describe_draws_no_value(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE SEQUENCE seq1", session=sess)
        ext = ExtendedSession(st, sess)
        assert self._describe(ext, "select nextval('seq1')")[:1] == b"T"
        assert one(st, "select nextval('seq1')", sess) == 1


class TestRegclass:
    @pytest.fixture
    def schemas(self, st):
        sess = Session(database=DB)
        run_sql(st, DB, "CREATE SCHEMA third_schema", session=sess)
        run_sql(st, DB, "CREATE TABLE third_schema.x (a int)", session=sess)
        run_sql(st, DB, "CREATE TABLE pub (a int)", session=sess)
        return sess

    def test_qualified_name_resolves_to_oid(self, st, schemas):
        assert one(st, "SELECT 'third_schema.x'::regclass::oid > 0", schemas) is True

    def test_join_against_pg_class_oid(self, st, schemas):
        rows = run_sql(
            st,
            DB,
            "SELECT c.relname FROM pg_catalog.pg_class c WHERE c.oid = 'third_schema.x'::regclass",
            session=schemas,
        )[-1].rows
        assert rows == [("x",)]

    def test_bare_name_follows_search_path(self, st, schemas):
        run_sql(st, DB, "SET search_path TO third_schema", session=schemas)
        assert one(st, "SELECT 'x'::regclass::oid > 0", schemas) is True

    def test_unknown_raises_42p01(self, st, schemas):
        with pytest.raises(errors.SQLError) as e:
            run_sql(st, DB, "SELECT 'no_such_rel'::regclass", session=schemas)
        assert e.value.sqlstate == "42P01"

    def test_renders_as_name(self, st, schemas):
        v = one(st, "SELECT 'pub'::regclass", schemas)
        assert typemap.to_pg_text(v, None) == b"pub"

    def test_regtype_rowtype_matches_pg_type(self, st, schemas):
        # pgjdbc's SearchPathLookupTest: TypeInfoCache resolves typname 'x'
        # through pg_type + namespace, and compares against
        # 'third_schema.x'::regtype::oid — both must yield the rowtype oid.
        run_sql(st, DB, "SET search_path TO third_schema", session=schemas)
        via_cast = one(st, "SELECT 'third_schema.x'::regtype::oid", schemas)
        via_catalog = one(
            st,
            "SELECT t.oid FROM pg_catalog.pg_type t "
            "JOIN pg_catalog.pg_namespace n ON t.typnamespace = n.oid "
            "WHERE t.typname = 'x' AND n.nspname = 'third_schema'",
            schemas,
        )
        assert via_cast == via_catalog

    def test_regtype_base_type(self, st, schemas):
        assert one(st, "SELECT 'int4'::regtype::oid", schemas) == 23
        v = one(st, "SELECT 'int4'::regtype", schemas)
        assert typemap.to_pg_text(v, None) == b"integer"


class TestSlashDates:
    def test_mdy_default(self, st):
        sess = Session(database=DB)
        typemap.set_render_session(sess)
        v = one(st, "SELECT '8/10/7777'::timestamp", sess)
        assert (v.year, v.month, v.day) == (7777, 8, 10)

    def test_dmy(self, st):
        sess = Session(database=DB)
        typemap.set_render_session(sess)
        run_sql(st, DB, "SET DateStyle = 'ISO, DMY'", session=sess)
        v = one(st, "SELECT '8/10/7777'::timestamp", sess)
        assert (v.year, v.month, v.day) == (7777, 10, 8)

    def test_with_time_component(self, st):
        sess = Session(database=DB)
        typemap.set_render_session(sess)
        v = one(st, "SELECT '8/10/2017 12:34:56'::timestamp", sess)
        assert (v.hour, v.minute, v.second) == (12, 34, 56)


class TestArrayFill:
    def test_one_dimensional(self, st):
        assert one(st, "SELECT array_fill(7, ARRAY[3])") == [7, 7, 7]

    def test_multi_dimensional(self, st):
        assert one(st, "SELECT array_fill(0, ARRAY[2,3])") == [[0, 0, 0], [0, 0, 0]]

    def test_unnest_of_fill(self, st):
        rows = run_sql(
            st,
            DB,
            "SELECT unnest(array_fill('8/10/2017'::timestamp, ARRAY[4]))",
            session=Session(database=DB),
        )[-1].rows
        assert len(rows) == 4
        assert rows[0][0].year == 2017


class TestGeoBinaryResults:
    def test_point_and_box_binary_encode(self, st):
        from secantus.sql.pgextended import _encode_value

        assert _encode_value("(1,2)", 600, None) == struct.pack("!2d", 1.0, 2.0)
        assert _encode_value("(3,4),(1,2)", 603, None) == struct.pack("!4d", 3.0, 4.0, 1.0, 2.0)

    def test_circle_and_line(self, st):
        from secantus.sql.pgextended import _encode_value

        assert _encode_value("<(0,0),5>", 718, None) == struct.pack("!3d", 0.0, 0.0, 5.0)
        assert _encode_value("{1,-1,0}", 628, None) == struct.pack("!3d", 1.0, -1.0, 0.0)
