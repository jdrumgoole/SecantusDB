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
            "create trigger trg instead of insert on t2 for each row execute procedure tfn()",
            "create trigger trg before truncate on t2 for each row execute procedure tfn()",
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


class TestTriggerKinds:
    """AFTER / BEFORE, ROW / STATEMENT, INSERT / UPDATE / DELETE triggers run
    as PostgreSQL runs them. The expected log is PostgreSQL 15's own output
    for the same statements."""

    SETUP = (
        "create table tk (id int primary key, t text)",
        "create table tk_log (op text, lvl text, wh text, old_id int, new_id int, new_t text)",
        "create function tk_audit() returns trigger as $$ begin insert into tk_log values "
        "(tg_op, tg_level, tg_when, old.id, new.id, new.t); return null; end $$ language plpgsql",
        "create function tk_upper() returns trigger as $$ begin new.t := upper(new.t); "
        "return new; end $$ language plpgsql",
        "create function tk_keep() returns trigger as $$ begin if old.id = 2 then "
        "return null; end if; return old; end $$ language plpgsql",
        "create function tk_stmt() returns trigger as $$ begin insert into tk_log values "
        "(tg_op, tg_level, tg_when, null, null, tg_table_name); return null; end $$ "
        "language plpgsql",
        "create trigger a1 after insert or update or delete on tk for each row "
        "execute function tk_audit()",
        "create trigger b1 before update on tk for each row execute function tk_upper()",
        "create trigger b2 before delete on tk for each row execute function tk_keep()",
        "create trigger s1 before insert or delete on tk for each statement "
        "execute function tk_stmt()",
        "create trigger s2 after update on tk execute function tk_stmt()",
    )

    def test_matches_postgres(self, storage, session):
        for sql in self.SETUP:
            run(storage, session, sql)
        assert (
            run(storage, session, "insert into tk values (1, 'a'), (2, 'b'), (3, 'c')").rowcount
            == 3
        )
        assert run(storage, session, "update tk set t = 'x' where id >= 2").rowcount == 2
        assert run(storage, session, "delete from tk").rowcount == 2
        run(storage, session, "update tk set t = 'y' where id = 99")
        assert run(storage, session, "select * from tk order by id").rows == [(2, "X")]
        assert run(storage, session, "select * from tk_log").rows == [
            ("INSERT", "STATEMENT", "BEFORE", None, None, "tk"),
            ("INSERT", "ROW", "AFTER", None, 1, "a"),
            ("INSERT", "ROW", "AFTER", None, 2, "b"),
            ("INSERT", "ROW", "AFTER", None, 3, "c"),
            ("UPDATE", "ROW", "AFTER", 2, 2, "X"),
            ("UPDATE", "ROW", "AFTER", 3, 3, "X"),
            ("UPDATE", "STATEMENT", "AFTER", None, None, "tk"),
            ("DELETE", "STATEMENT", "BEFORE", None, None, "tk"),
            ("DELETE", "ROW", "AFTER", 1, None, None),
            ("DELETE", "ROW", "AFTER", 3, None, None),
            ("UPDATE", "STATEMENT", "AFTER", None, None, "tk"),
        ]

    def test_when_conditions_match_postgres(self, storage, session):
        for sql in (
            "create table tw (id int primary key, t text)",
            "create table tw_log (op text, id int, t text)",
            "create function tw_fn() returns trigger as $$ begin insert into tw_log values "
            "(tg_op, coalesce(new.id, old.id), new.t); return null; end $$ language plpgsql",
            "create trigger w1 after insert on tw for each row when (new.id > 1) "
            "execute function tw_fn()",
            "create trigger w2 after update on tw for each row "
            "when (old.t is distinct from new.t) execute function tw_fn()",
            "insert into tw values (1, 'a'), (2, 'b'), (3, null)",
            "update tw set t = 'b' where id <= 2",
            "update tw set t = 'c' where id = 3",
        ):
            run(storage, session, sql)
        assert run(storage, session, "select * from tw_log").rows == [
            ("INSERT", 2, "b"),
            ("INSERT", 3, None),
            ("UPDATE", 1, "b"),
            ("UPDATE", 3, "c"),
        ]

    def test_a_failing_statement_takes_its_triggers_writes_with_it(self, storage, session):
        for sql in self.SETUP:
            run(storage, session, sql)
        run(storage, session, "insert into tk values (1, 'a')")
        before = run(storage, session, "select count(*) from tk_log").rows
        with pytest.raises(SQLError) as ei:
            run(storage, session, "insert into tk values (1, 'dup')")
        assert ei.value.sqlstate == "23505"
        # The BEFORE STATEMENT trigger logged a row; it rolled back with the
        # INSERT, as in PostgreSQL.
        assert run(storage, session, "select count(*) from tk_log").rows == before


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
            "args": extra.pop("args", []),
            "update_columns": extra.pop("update_columns", []),
            **extra,
        },
    )


class TestTriggersThisServerCannotRun:
    """A trigger the Rust server stored that this server cannot run refuses
    the write it would fire on (0A000) rather than being skipped: UPDATE OF,
    transition tables, constraint triggers, and every path that fires nothing (ON CONFLICT ...)."""

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

    def test_unrunnable_shapes_refuse_their_writes(self, rt, session):
        _rust_trigger(rt, "trans", "AFTER", ["INSERT"], level="STATEMENT", transition_new="nt")
        self._refused(rt, session, "insert into rt (id, t) values (2, 'b')")
        assert run(rt, session, "select t from rt").rows == [("a",)]

    def test_a_multi_event_rust_trigger_fires(self, rt, session):
        # Recorded with `event` = UPDATE (the first) and both in `events`:
        # reading only `event` skipped it on INSERT.
        _rust_trigger(rt, "both", "BEFORE", ["UPDATE", "INSERT"])
        run(rt, session, "insert into rt (id, t) values (2, 'cats')")
        run(rt, session, "update rt set t = 'dogs' where id = 2")
        assert run(rt, session, "select ts::text from rt where id = 2").rows == [("'dog':1",)]

    def test_on_conflict_with_a_transition_table_refuses(self, rt, session):
        _rust_trigger(rt, "ins", "AFTER", ["INSERT"], level="STATEMENT", transition_new="nt")
        self._refused(
            rt, session, "insert into rt (id, t) values (1, 'b') on conflict (id) do nothing"
        )


class TestArgsUpdateOfTruncate:
    """Trigger arguments (TG_ARGV from 0, TG_NARGS), UPDATE OF column lists
    (fire when a listed column is a SET target, changed or not) and TRUNCATE
    statement triggers. The expected log is PostgreSQL 15's verbatim."""

    def test_matches_postgres(self, storage, session):
        for sql in (
            "create table tb18_t(id int primary key, a int, b int)",
            "create table tb18_log(msg text)",
            "create function tb18_f() returns trigger language plpgsql as $$ begin "
            "insert into tb18_log values (tg_name||':'||tg_op||':'||tg_level||':'||tg_nargs"
            "||':'||coalesce(tg_argv[0],'-')||':'||coalesce(tg_argv[1],'-')); "
            "return null; end $$",
            "create trigger tb18_tr before truncate on tb18_t for each statement "
            "execute function tb18_f('x', 7)",
            "create trigger tb18_ua after update of a on tb18_t for each row "
            "execute function tb18_f()",
            "create trigger tb18_us after update of b on tb18_t for each statement "
            "execute function tb18_f('-3')",
            "insert into tb18_t values (1,1,1),(2,2,2)",
            "update tb18_t set a = a",
            "update tb18_t set b = 5 where id = 1",
            "update tb18_t set id = id",
            "truncate tb18_t",
        ):
            run(storage, session, sql)
        assert run(storage, session, "select msg from tb18_log").rows == [
            ("tb18_ua:UPDATE:ROW:0:-:-",),
            ("tb18_ua:UPDATE:ROW:0:-:-",),
            ("tb18_us:UPDATE:STATEMENT:1:-3:-",),
            ("tb18_tr:TRUNCATE:STATEMENT:2:x:7",),
        ]
        assert run(storage, session, "select count(*) from tb18_t").rows == [(0,)]

    def test_a_raising_truncate_trigger_keeps_the_rows(self, storage, session):
        run(storage, session, "create table tb18_k(id int)")
        run(storage, session, "insert into tb18_k values (1)")
        run(
            storage,
            session,
            "create function tb18_no() returns trigger language plpgsql as "
            "$$ begin raise exception 'no'; end $$",
        )
        run(
            storage,
            session,
            "create trigger tb18_g after truncate on tb18_k execute function tb18_no()",
        )
        with pytest.raises(SQLError):
            run(storage, session, "truncate tb18_k")
        assert run(storage, session, "select count(*) from tb18_k").rows == [(1,)]


ON_CONFLICT_SQL = """\
create table tb19_t(id int primary key, v text);
create table tb19_log(msg text);
create function tb19_f() returns trigger language plpgsql as $$ begin
insert into tb19_log values (
  tg_name||':'||tg_when||':'||tg_op||':'||tg_level
  ||':'||coalesce(old.v,'-')||'>'||coalesce(new.v,'-'));
if tg_level = 'ROW' and tg_when = 'BEFORE' then
  if new.v = 'skip' then return null; end if;
  new.v := new.v || '!'; return new;
end if;
return null; end $$;
create function tb19_s() returns trigger language plpgsql as $$ begin
insert into tb19_log values (
  tg_name||':'||tg_when||':'||tg_op||':'||tg_level); return null; end $$;
create trigger a_bi before insert on tb19_t for each row execute function tb19_f();
create trigger b_bu before update on tb19_t for each row execute function tb19_f();
create trigger c_ai after insert on tb19_t for each row execute function tb19_f();
create trigger d_au after update on tb19_t for each row execute function tb19_f();
create trigger e_bis before insert on tb19_t for each statement execute function tb19_s();
create trigger f_bus before update on tb19_t for each statement execute function tb19_s();
create trigger g_ais after insert on tb19_t for each statement execute function tb19_s();
create trigger h_aus after update on tb19_t for each statement execute function tb19_s();
insert into tb19_t values (1, 'a');
delete from tb19_log;
insert into tb19_t values (1, 'b'), (2, 'c'), (3, 'skip')
  on conflict (id) do update set v = excluded.v || '+';
insert into tb19_log values ('---');
insert into tb19_t values (1, 'x'), (4, 'd') on conflict do nothing;
select msg from tb19_log;
select * from tb19_t order by id;
"""

UPDATE_FROM_SQL = """\
create table tb19_t(id int primary key, v text);
create table tb19_s(id int, w text);
create table tb19_log(msg text);
create function tb19_f() returns trigger language plpgsql as $$ begin
insert into tb19_log values (
  tg_name||':'||tg_when||':'||tg_op||':'||tg_level
  ||':'||coalesce(old.v,'-')||'>'||coalesce(new.v,'-'));
if tg_when = 'BEFORE' and tg_op = 'UPDATE' then
  if new.v = 'skip' then return null; end if;
  new.v := new.v || '!'; return new;
end if;
if tg_when = 'BEFORE' and tg_op = 'DELETE' then
  if old.v = 'keep!' then return null; end if;
  return old;
end if;
return null; end $$;
create function tb19_st() returns trigger language plpgsql as $$ begin
insert into tb19_log values (
  tg_name||':'||tg_when||':'||tg_op||':'||tg_level); return null; end $$;
create trigger a_bu before update of v on tb19_t for each row execute function tb19_f();
create trigger b_au after update on tb19_t for each row execute function tb19_f();
create trigger c_bd before delete on tb19_t for each row execute function tb19_f();
create trigger d_ad after delete on tb19_t for each row execute function tb19_f();
create trigger e_us before update on tb19_t for each statement execute function tb19_st();
create trigger f_ds after delete on tb19_t for each statement execute function tb19_st();
insert into tb19_t values (1, 'a'), (2, 'b'), (3, 'c');
insert into tb19_s values (1, 'x'), (2, 'skip'), (3, 'keep');
update tb19_t set v = s.w from tb19_s s where s.id = tb19_t.id;
insert into tb19_log values ('---');
delete from tb19_t using tb19_s s where s.id = tb19_t.id;
select msg from tb19_log;
select * from tb19_t order by id;
"""

MERGE_SQL = """\
create table tb19_t(id int primary key, v text);
create table tb19_s(id int, w text);
create table tb19_log(msg text);
create function tb19_f() returns trigger language plpgsql as $$ begin
insert into tb19_log values (
  tg_name||':'||tg_when||':'||tg_op||':'||tg_level
  ||':'||coalesce(old.v,'-')||'>'||coalesce(new.v,'-'));
if tg_when = 'BEFORE' and tg_op in ('UPDATE','INSERT') then
  if new.v = 'skip' then return null; end if;
  new.v := new.v || '!'; return new;
end if;
if tg_when = 'BEFORE' and tg_op = 'DELETE' then
  if old.v = 'keep' then return null; end if;
  return old;
end if;
return null; end $$;
create function tb19_st() returns trigger language plpgsql as $$ begin
insert into tb19_log values (
  tg_name||':'||tg_when||':'||tg_op||':'||tg_level); return null; end $$;
create trigger r_bi before insert on tb19_t for each row execute function tb19_f();
create trigger r_bu before update on tb19_t for each row execute function tb19_f();
create trigger r_bd before delete on tb19_t for each row execute function tb19_f();
create trigger r_ai after insert on tb19_t for each row execute function tb19_f();
create trigger r_au after update on tb19_t for each row execute function tb19_f();
create trigger r_ad after delete on tb19_t for each row execute function tb19_f();
create trigger s_bi before insert on tb19_t for each statement execute function tb19_st();
create trigger s_bu before update on tb19_t for each statement execute function tb19_st();
create trigger s_bd before delete on tb19_t for each statement execute function tb19_st();
create trigger s_ai after insert on tb19_t for each statement execute function tb19_st();
create trigger s_au after update on tb19_t for each statement execute function tb19_st();
create trigger s_ad after delete on tb19_t for each statement execute function tb19_st();
insert into tb19_t values (1, 'a'), (2, 'b'), (3, 'del'), (4, 'keep');
delete from tb19_log;
insert into tb19_s values (1, 'x'), (2, 'skip'), (3, 'D'), (4, 'D'), (5, 'n'), (6, 'skip');
merge into tb19_t t using tb19_s s on t.id = s.id
  when matched and s.w = 'D' then delete
  when matched then update set v = s.w
  when not matched then insert values (s.id, s.w);
insert into tb19_log values ('---');
merge into tb19_t t using tb19_s s on t.id = s.id
  when not matched then insert values (s.id + 10, s.w);
select msg from tb19_log;
select * from tb19_t order by id;
"""


def _scenario(storage, session, sql):
    """Run a multi-statement scenario; the last two SELECTs' rows."""
    results = run_sql(storage, DB, sql, session=session)
    return results[-2].rows, results[-1].rows


class TestTriggersOnEveryWritePath:
    """ON CONFLICT, UPDATE FROM / DELETE USING and MERGE fire triggers in
    PostgreSQL's order: BEFORE STATEMENT per event, BEFORE ROW as each row is
    acted on (its NEW is what ON CONFLICT's EXCLUDED sees), AFTER ROW queued
    to the end of the statement, AFTER STATEMENT in reverse. Each expected
    log and table is PostgreSQL 15's output for the same script."""

    def test_on_conflict(self, storage, session):
        log, rows = _scenario(
            storage,
            session,
            ON_CONFLICT_SQL,
        )
        assert log == [
            ("e_bis:BEFORE:INSERT:STATEMENT",),
            ("f_bus:BEFORE:UPDATE:STATEMENT",),
            ("a_bi:BEFORE:INSERT:ROW:->b",),
            ("b_bu:BEFORE:UPDATE:ROW:a!>b!+",),
            ("a_bi:BEFORE:INSERT:ROW:->c",),
            ("a_bi:BEFORE:INSERT:ROW:->skip",),
            ("d_au:AFTER:UPDATE:ROW:a!>b!+!",),
            ("c_ai:AFTER:INSERT:ROW:->c!",),
            ("h_aus:AFTER:UPDATE:STATEMENT",),
            ("g_ais:AFTER:INSERT:STATEMENT",),
            ("---",),
            ("e_bis:BEFORE:INSERT:STATEMENT",),
            ("a_bi:BEFORE:INSERT:ROW:->x",),
            ("a_bi:BEFORE:INSERT:ROW:->d",),
            ("c_ai:AFTER:INSERT:ROW:->d!",),
            ("g_ais:AFTER:INSERT:STATEMENT",),
        ]
        assert rows == [(1, "b!+!"), (2, "c!"), (4, "d!")]

    def test_update_from_delete_using(self, storage, session):
        log, rows = _scenario(
            storage,
            session,
            UPDATE_FROM_SQL,
        )
        assert log == [
            ("e_us:BEFORE:UPDATE:STATEMENT",),
            ("a_bu:BEFORE:UPDATE:ROW:a>x",),
            ("a_bu:BEFORE:UPDATE:ROW:b>skip",),
            ("a_bu:BEFORE:UPDATE:ROW:c>keep",),
            ("b_au:AFTER:UPDATE:ROW:a>x!",),
            ("b_au:AFTER:UPDATE:ROW:c>keep!",),
            ("---",),
            ("c_bd:BEFORE:DELETE:ROW:x!>-",),
            ("c_bd:BEFORE:DELETE:ROW:b>-",),
            ("c_bd:BEFORE:DELETE:ROW:keep!>-",),
            ("d_ad:AFTER:DELETE:ROW:x!>-",),
            ("d_ad:AFTER:DELETE:ROW:b>-",),
            ("f_ds:AFTER:DELETE:STATEMENT",),
        ]
        assert rows == [(3, "keep!")]

    def test_merge(self, storage, session):
        log, rows = _scenario(
            storage,
            session,
            MERGE_SQL,
        )
        assert log == [
            ("s_bi:BEFORE:INSERT:STATEMENT",),
            ("s_bu:BEFORE:UPDATE:STATEMENT",),
            ("s_bd:BEFORE:DELETE:STATEMENT",),
            ("r_bu:BEFORE:UPDATE:ROW:a!>x",),
            ("r_bu:BEFORE:UPDATE:ROW:b!>skip",),
            ("r_bd:BEFORE:DELETE:ROW:del!>-",),
            ("r_bd:BEFORE:DELETE:ROW:keep!>-",),
            ("r_bi:BEFORE:INSERT:ROW:->n",),
            ("r_bi:BEFORE:INSERT:ROW:->skip",),
            ("r_au:AFTER:UPDATE:ROW:a!>x!",),
            ("r_ad:AFTER:DELETE:ROW:del!>-",),
            ("r_ad:AFTER:DELETE:ROW:keep!>-",),
            ("r_ai:AFTER:INSERT:ROW:->n!",),
            ("s_ad:AFTER:DELETE:STATEMENT",),
            ("s_au:AFTER:UPDATE:STATEMENT",),
            ("s_ai:AFTER:INSERT:STATEMENT",),
            ("---",),
            ("s_bi:BEFORE:INSERT:STATEMENT",),
            ("r_bi:BEFORE:INSERT:ROW:->D",),
            ("r_bi:BEFORE:INSERT:ROW:->D",),
            ("r_bi:BEFORE:INSERT:ROW:->skip",),
            ("r_ai:AFTER:INSERT:ROW:->D!",),
            ("r_ai:AFTER:INSERT:ROW:->D!",),
            ("s_ai:AFTER:INSERT:STATEMENT",),
        ]
        assert rows == [(1, "x!"), (2, "b!"), (5, "n!"), (13, "D!"), (14, "D!")]
