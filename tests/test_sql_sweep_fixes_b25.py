"""Python PG server divergences found by sweeping every PostgreSQL corpus
(`tools/probes/pg_corpora/`) against it on 2026-10-01. Each expected value
is PostgreSQL 15's for the same statement."""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def fails(storage, session, sql):
    with pytest.raises(SQLError) as ei:
        run(storage, session, sql)
    return ei.value


class TestJoinAliasNamedLikeABaseColumn:
    """`FROM a o JOIN a n`: the base column `n` and the relation `n` were one
    key in the joined row, so `o.n` read the whole embedded row."""

    def test_the_qualified_column_reads_its_value(self, storage, session):
        run(storage, session, "create table a(id int primary key, n int)")
        run(storage, session, "insert into a values (1, 10)")
        sql = "select o.id || ' ' || o.n || '->' || n.n from a o join a n on n.id = o.id"
        assert run(storage, session, sql).rows == [("1 10->10",)]
        assert run(storage, session, "select n.* from a o join a n on n.id = o.id").rows == [
            (1, 10)
        ]
        assert fails(storage, session, "select n from a o join a n on n.id = o.id").sqlstate == (
            "42702"
        )


class TestIsDistinctFrom:
    @pytest.fixture
    def dz(self, storage, session):
        run(storage, session, "create table dz(id int primary key, a int, b int)")
        run(
            storage,
            session,
            "insert into dz values (1,1,1),(2,null,null),(3,1,null),(4,null,2),(5,1,2)",
        )
        return storage

    @pytest.mark.parametrize(
        ("sql", "expected"),
        [
            ("select id from dz where a is distinct from b order by id", [(3,), (4,), (5,)]),
            ("select id from dz where a is not distinct from b order by id", [(1,), (2,)]),
            ("select id from dz where a is distinct from 1 order by id", [(2,), (4,)]),
            ("select id from dz where b is not distinct from null order by id", [(2,), (3,)]),
            (
                "select x.id, y.id from dz x join dz y on x.a is not distinct from y.b "
                "where x.id < 3 and y.id < 4 order by 1, 2",
                [(1, 1), (2, 2), (2, 3)],
            ),
            ("delete from dz where a is not distinct from null returning id", [(2,), (4,)]),
        ],
    )
    def test_matches_postgres(self, dz, session, sql, expected):
        assert sorted(run(dz, session, sql).rows) == sorted(expected)


def test_string_agg_of_an_expression_over_a_join(storage, session):
    run(storage, session, "create table lj(id int primary key)")
    run(storage, session, "create table lk(id int primary key, jid int, v int)")
    run(storage, session, "insert into lj values (1), (2)")
    run(storage, session, "insert into lk values (1, 1, 5), (2, 1, 7)")
    sql = (
        "select j.id, string_agg(k.v::text, ',') from lj j left join lk k on k.jid = j.id "
        "group by j.id order by j.id"
    )
    assert run(storage, session, sql).rows == [(1, "5,7"), (2, None)]


class TestCreateOrReplaceView:
    @pytest.fixture
    def v(self, storage, session):
        run(storage, session, "create table vx(id int, a int, b text)")
        run(storage, session, "create view vxv as select id, a from vx")
        return storage

    @pytest.mark.parametrize(
        ("body", "message"),
        [
            ("select id from vx", "cannot drop columns from view"),
            (
                "select id, b as a from vx",
                'cannot change data type of view column "a" from integer to text',
            ),
            ("select id, a as z from vx", 'cannot change name of view column "a" to "z"'),
        ],
    )
    def test_refuses_what_postgres_refuses(self, v, session, body, message):
        e = fails(v, session, f"create or replace view vxv as {body}")
        assert (e.sqlstate, str(e)) == ("42P16", message)

    def test_adding_a_column_at_the_end_is_allowed(self, v, session):
        run(v, session, "create or replace view vxv as select id, a, b from vx")
        assert [c.name for c in run(v, session, "select * from vxv").columns] == ["id", "a", "b"]


class TestDateInput:
    @pytest.mark.parametrize(
        "text",
        ["Jan 5, 2020", "January 5 2020", "5 Jan 2020", "2020-Jan-05", "Jan-05-2020",
         "Mon Jan 5 2020", "Jan 5, 20"],
    )  # fmt: skip
    def test_month_names(self, storage, session, text):
        assert run(storage, session, f"select '{text}'::date::text").rows == [("2020-01-05",)]

    @pytest.mark.parametrize("text", ["Jan 32 2020", "Feb 29 2021", "2020-02-30", "2020-13-01"])
    def test_a_field_out_of_range_is_22008(self, storage, session, text):
        e = fails(storage, session, f"select '{text}'::date")
        assert (e.sqlstate, str(e)) == ("22008", f'date/time field value out of range: "{text}"')

    def test_garbage_is_22007(self, storage, session):
        assert fails(storage, session, "select 'Jan 5'::date").sqlstate == "22007"


def test_a_whole_row_reference_describes_as_the_row_type(storage, session):
    run(storage, session, "create table jr(id int primary key, a int, b text)")
    run(storage, session, "insert into jr values (1, 10, 'x')")
    col = run(storage, session, "select jr from jr where id = 1").columns[0]
    assert col.pg_oid != 2249  # not the generic record


class TestTriggerCatalog:
    def test_pg_trigger_lists_constraint_triggers(self, storage, session):
        run(storage, session, "create table ct(id int)")
        run(
            storage,
            session,
            "create function ctf() returns trigger language plpgsql as "
            "$$ begin return null; end $$",
        )
        run(
            storage,
            session,
            "create constraint trigger ct_def after insert on ct deferrable initially deferred "
            "for each row execute function ctf()",
        )
        run(
            storage,
            session,
            "create constraint trigger ct_imm after insert on ct for each row "
            "execute function ctf()",
        )
        sql = (
            "select tgname, tgdeferrable, tginitdeferred, tgconstraint <> 0 from pg_trigger "
            "where tgrelid = 'ct'::regclass order by 1"
        )
        assert run(storage, session, sql).rows == [
            ("ct_def", True, True, True),
            ("ct_imm", False, False, True),
        ]

    def test_set_constraints_on_an_unknown_name_is_42704(self, storage, session):
        run(storage, session, "begin")
        e = fails(storage, session, "set constraints nosuch deferred")
        assert (e.sqlstate, str(e)) == ("42704", 'constraint "nosuch" does not exist')

    def test_transition_tables_need_two_names(self, storage, session):
        run(storage, session, "create table tt(id int)")
        run(
            storage,
            session,
            "create function ttf() returns trigger language plpgsql as "
            "$$ begin return null; end $$",
        )
        e = fails(
            storage,
            session,
            "create trigger b after update on tt referencing new table as x old table as x "
            "for each statement execute function ttf()",
        )
        assert (e.sqlstate, str(e)) == (
            "42P17",
            "OLD TABLE name and NEW TABLE name cannot be the same",
        )
