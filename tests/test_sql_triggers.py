"""BEFORE INSERT FOR EACH ROW triggers — the supported CREATE TRIGGER shape.

The pgx tsvector-maintenance shape: a plpgsql ``RETURNS trigger`` function
reads and mutates its NEW record (``new.ts := to_tsvector(new.t)``), a
``CREATE TRIGGER … BEFORE INSERT … FOR EACH ROW EXECUTE PROCEDURE`` binds it
to a table, and every insert path (INSERT and COPY) runs the rows through it.
``RETURN NULL`` skips the row, like real PG. Everything else — AFTER,
UPDATE/DELETE events, statement-level — stays faithfully rejected.
"""

from __future__ import annotations

import psycopg
import pytest

from secantus.sql import run_sql
from secantus.sql.errors import SQLError
from secantus.sql.pgserver import SecantusPGServer
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "d"

TRIGGER_FN = """create function tfn() returns trigger as $$
begin
  new.ts := to_tsvector(new.t);
  return new;
end
$$ language plpgsql"""


@pytest.fixture()
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


@pytest.fixture()
def session():
    return Session(database=DB)


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


class TestDDL:
    def test_create_requires_trigger_function(self, storage, session):
        run(storage, session, "create table t1 (t text, ts tsvector)")
        run(
            storage,
            session,
            "create function plain() returns integer as $$ select 1 $$ language sql",
        )
        with pytest.raises(SQLError) as ei:
            run(
                storage,
                session,
                "create trigger trg before insert on t1 for each row execute procedure plain()",
            )
        assert ei.value.sqlstate == "42P17"

    def test_unsupported_shapes_rejected(self, storage, session):
        run(storage, session, "create table t2 (t text)")
        run(storage, session, TRIGGER_FN)
        for ddl in (
            "create trigger trg after insert on t2 for each row execute procedure tfn()",
            "create trigger trg before update on t2 for each row execute procedure tfn()",
            "create trigger trg before insert on t2 execute procedure tfn()",
        ):
            with pytest.raises(SQLError) as ei:
                run(storage, session, ddl)
            assert ei.value.sqlstate == "0A000"

    def test_duplicate_trigger_is_42710(self, storage, session):
        run(storage, session, "create table t3 (t text, ts tsvector)")
        run(storage, session, TRIGGER_FN)
        run(
            storage,
            session,
            "create trigger trg before insert on t3 for each row execute procedure tfn()",
        )
        with pytest.raises(SQLError) as ei:
            run(
                storage,
                session,
                "create trigger trg before insert on t3 for each row execute procedure tfn()",
            )
        assert ei.value.sqlstate == "42710"

    def test_missing_function_is_42883(self, storage, session):
        run(storage, session, "create table t4 (t text)")
        with pytest.raises(SQLError) as ei:
            run(
                storage,
                session,
                "create trigger trg before insert on t4 for each row execute procedure nope()",
            )
        assert ei.value.sqlstate == "42883"


class TestFiring:
    def _setup(self, storage, session):
        run(storage, session, "create table s1 (t text, ts tsvector)")
        run(storage, session, TRIGGER_FN)
        run(
            storage,
            session,
            "create trigger trg before insert on s1 for each row execute procedure tfn()",
        )

    def test_new_mutation_lands_in_row(self, storage, session):
        self._setup(storage, session)
        run(storage, session, "insert into s1 (t) values ('the cat sat')")
        rows = run(storage, session, "select ts from s1").rows
        assert rows == [({"tsvector": {"cat": [2], "sat": [3]}},)]

    def test_multi_row_insert_fires_per_row(self, storage, session):
        self._setup(storage, session)
        run(storage, session, "insert into s1 (t) values ('red fox'), ('blue jay')")
        rows = run(storage, session, "select t, ts from s1 order by t").rows
        assert rows[0][1] == {"tsvector": {"blue": [1], "jay": [2]}}
        assert rows[1][1] == {"tsvector": {"red": [1], "fox": [2]}}

    def test_return_null_skips_row(self, storage, session):
        run(storage, session, "create table s2 (n int4)")
        run(
            storage,
            session,
            """create function oddonly() returns trigger as $$
begin
  if new.n % 2 = 0 then
    return null;
  end if;
  return new;
end
$$ language plpgsql""",
        )
        run(
            storage,
            session,
            "create trigger trg before insert on s2 for each row execute procedure oddonly()",
        )
        res = run(storage, session, "insert into s2 values (1), (2), (3), (4)")
        assert res.rowcount == 2
        assert run(storage, session, "select n from s2 order by n").rows == [(1,), (3,)]

    def test_trigger_dies_with_table(self, storage, session):
        self._setup(storage, session)
        run(storage, session, "drop table s1")
        run(storage, session, "create table s1 (t text, ts tsvector)")
        run(storage, session, "insert into s1 (t) values ('quick brown fox')")
        assert run(storage, session, "select ts from s1").rows == [(None,)]

    def test_drop_trigger_stops_firing(self, storage, session):
        self._setup(storage, session)
        run(storage, session, "insert into s1 (t) values ('the cat')")
        assert run(storage, session, "drop trigger trg on s1").command_tag == "DROP TRIGGER"
        # After the drop the trigger no longer fires — ts stays NULL.
        run(storage, session, "insert into s1 (t) values ('the dog')")
        rows = run(storage, session, "select t, ts from s1 order by t").rows
        assert rows == [("the cat", {"tsvector": {"cat": [2]}}), ("the dog", None)]

    def test_drop_missing_trigger_is_42704(self, storage, session):
        run(storage, session, "create table s3 (n int4)")
        with pytest.raises(SQLError) as ei:
            run(storage, session, "drop trigger nope on s3")
        assert ei.value.sqlstate == "42704"

    def test_drop_trigger_if_exists(self, storage, session):
        run(storage, session, "create table s4 (n int4)")
        assert (
            run(storage, session, "drop trigger if exists nope on s4").command_tag == "DROP TRIGGER"
        )

    def test_overlong_words_index_empty_like_pg(self, storage, session):
        self._setup(storage, session)
        big = "x" * 10001
        run(storage, session, f"insert into s1 (t) values ('{big}')")
        assert run(storage, session, "select ts from s1").rows == [({"tsvector": {}},)]


@pytest.fixture()
def server(tmp_path):
    srv = SecantusPGServer(storage_path=str(tmp_path), port=0)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture()
def dsn(server):
    host, port = server.address
    return f"host={host} port={port} dbname=test user=test password=test"


class TestWire:
    def test_pg_temp_trigger_fires_through_copy(self, dsn):
        # The pgx TestConnCopyFromNoticeResponseReceivedMidStream shape.
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("create temporary table sentences(t text, ts tsvector)")
            c.execute(
                """create function pg_temp.sentences_trigger() returns trigger as $$
begin
  new.ts := to_tsvector(new.t);
  return new;
end
$$ language plpgsql"""
            )
            c.execute(
                "create trigger sentences_update before insert on sentences "
                "for each row execute procedure pg_temp.sentences_trigger()"
            )
            with c.cursor() as cur, cur.copy("COPY sentences(t) FROM STDIN") as copy:
                copy.write("the cat sat\nbig red dog\n")
            got = c.execute("select t from sentences where ts @@ to_tsquery('cat')").fetchall()
            assert got == [("the cat sat",)]


def _rust_trigger(storage, name, timing, events, level="ROW", **extra):
    """A trigger as the Rust PG server records it in the shared catalog."""
    from secantus.sql.catalog import Catalog

    Catalog(storage).put_trigger(
        DB,
        {
            "name": name,
            "table": "rt",
            "timing": timing,
            "event": events[0],
            "events": events,
            "level": level,
            "function": "tfn",
            "args": [],
            "update_columns": [],
            **extra,
        },
    )


class TestTriggersThisServerCannotRun:
    """A trigger the Rust server stored refuses the write it would fire on.

    This server runs only BEFORE INSERT FOR EACH ROW triggers. It used to
    SKIP every other kind the shared catalog holds, so a write here bypassed
    an AFTER / UPDATE / DELETE / statement-level trigger the Rust server
    enforces. Now that write is PostgreSQL's 0A000 and does not happen.
    """

    @pytest.fixture()
    def rt(self, storage, session):
        run(storage, session, TRIGGER_FN)
        run(storage, session, "create table rt (id int primary key, t text, ts tsvector)")
        run(storage, session, "insert into rt (id, t) values (1, 'a')")
        return storage

    def _refused(self, storage, session, sql):
        with pytest.raises(SQLError) as ei:
            run(storage, session, sql)
        assert ei.value.sqlstate == "0A000"
        assert "cannot run on this server" in str(ei.value)

    def test_after_insert_refuses_the_insert(self, rt, session):
        _rust_trigger(rt, "audit", "AFTER", ["INSERT"])
        self._refused(rt, session, "insert into rt (id, t) values (2, 'b')")
        assert run(rt, session, "select count(*) from rt").rows == [(1,)]

    def test_update_and_delete_triggers_refuse_those_writes(self, rt, session):
        _rust_trigger(rt, "upd", "BEFORE", ["UPDATE"])
        _rust_trigger(rt, "del", "AFTER", ["DELETE"], level="STATEMENT")
        self._refused(rt, session, "update rt set t = 'z' where id = 1")
        self._refused(rt, session, "delete from rt where id = 1")
        assert run(rt, session, "select t from rt").rows == [("a",)]
        # An INSERT fires neither, so it goes through.
        run(rt, session, "insert into rt (id, t) values (2, 'b')")

    def test_truncate_trigger_refuses_truncate(self, rt, session):
        _rust_trigger(rt, "trunc", "BEFORE", ["TRUNCATE"], level="STATEMENT")
        self._refused(rt, session, "truncate rt")
        assert run(rt, session, "select count(*) from rt").rows == [(1,)]

    def test_statement_level_and_when_triggers_are_not_run_per_row(self, rt, session):
        _rust_trigger(rt, "stmt", "BEFORE", ["INSERT"], level="STATEMENT")
        self._refused(rt, session, "insert into rt (id, t) values (2, 'b')")
        run(rt, session, "drop trigger stmt on rt")
        _rust_trigger(rt, "cond", "BEFORE", ["INSERT"], when="new.id > 5")
        self._refused(rt, session, "insert into rt (id, t) values (2, 'b')")

    def test_a_multi_event_before_row_trigger_fires_on_insert(self, rt, session):
        # Recorded with `event` = UPDATE (the first) and both in `events`:
        # reading only `event` skipped it on INSERT.
        _rust_trigger(rt, "both", "BEFORE", ["UPDATE", "INSERT"])
        run(rt, session, "insert into rt (id, t) values (2, 'cats')")
        assert run(rt, session, "select ts::text from rt where id = 2").rows == [("'cat':1",)]
        self._refused(rt, session, "update rt set t = 'x' where id = 2")

    def test_on_conflict_runs_no_trigger_so_refuses(self, rt, session):
        _rust_trigger(rt, "ins", "BEFORE", ["INSERT"])
        self._refused(
            rt, session, "insert into rt (id, t) values (1, 'b') on conflict (id) do nothing"
        )
