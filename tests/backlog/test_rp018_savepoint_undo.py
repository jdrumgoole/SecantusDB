"""RP-018: a SAVEPOINT's cost is what is written after it, not the size of
the tables written.

A savepoint here once copied every table a later statement wrote, whole,
on that statement's first write: a `ROLLBACK TO` over one row of a large
table cost the table. It is now an undo journal of rows, as PostgreSQL's
subtransaction is a set of row versions: the first write of each row after
a savepoint records the row as it was (or that it did not exist), and a
`ROLLBACK TO` that cannot move the block onto a fresh transaction puts back
those rows only.

Each scenario runs against a private, freshly initdb'd PostgreSQL 15.19
cluster and this server, and every one runs twice: as written, where the
`ROLLBACK TO` moves the block (`rollback_to_by_move`), and after a DROP
TABLE in the same block, which no move can replay, so the undo journal does
the work. The literals are 15.19's answers, measured by running this file
as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp018_savepoint_undo \
        "host=127.0.0.1 port=5497 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made. A statement starting `!` is answered with its DETAIL, `@b` runs on a
second connection, and `%copy <table>|<rows>` runs `COPY <table> FROM
STDIN` with the given text rows.
"""

from __future__ import annotations

import sys
import time
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_batch34 import _answer

_SETUP = [
    "create table rp018 (a int primary key, b int, u int unique, t text)",
    "create index rp018_b on rp018 (b)",
    "insert into rp018 select g, g * 10, g, 'r' || g from generate_series(1, 5) g",
]
_ROWS = "select * from rp018 order by a"
_BY_B = "select a from rp018 where b = 20"
_CLEANUP = ["drop table rp018"]

# A statement no move can replay (a DROP), made in the block before its
# savepoints: every ROLLBACK TO after it is the undo journal's.
_FORCE_SETUP = "create table rp018_force (x int)"
_FORCE = "drop table rp018_force"

# (statements before BEGIN, statements inside the block after BEGIN up to
# and including COMMIT/ROLLBACK, statements after).
SCENARIOS: dict[str, tuple[list[str], list[str], list[str]]] = {
    "update, insert and delete undone": (
        _SETUP,
        [
            "savepoint s",
            "update rp018 set b = b + 1 where a <= 2",
            "insert into rp018 values (6, 60, 6, 'r6')",
            "delete from rp018 where a = 5",
            _ROWS,
            "rollback to s",
            _ROWS,
            _BY_B,
            "select count(*) from rp018 where u = 5",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "one row written many times": (
        _SETUP,
        [
            "savepoint s",
            "update rp018 set b = 1 where a = 1",
            "update rp018 set b = 2 where a = 1",
            "delete from rp018 where a = 1",
            "insert into rp018 values (1, 99, 1, 'new')",
            "rollback to s",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "nested savepoints rolled back inner then outer": (
        _SETUP,
        [
            "update rp018 set t = 'before' where a = 3",
            "savepoint s1",
            "update rp018 set b = -1 where a = 1",
            "savepoint s2",
            "update rp018 set b = -2 where a in (1, 2)",
            "insert into rp018 values (7, 70, 7, 'r7')",
            "rollback to s2",
            _ROWS,
            "update rp018 set b = -3 where a = 4",
            "rollback to s1",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "RELEASE then the outer ROLLBACK TO undoes both": (
        _SETUP,
        [
            "savepoint s1",
            "update rp018 set b = 0 where a = 1",
            "savepoint s2",
            "update rp018 set b = 0 where a = 2",
            "delete from rp018 where a = 1",
            "release s2",
            _ROWS,
            "rollback to s1",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "ROLLBACK TO, more writes, ROLLBACK TO again": (
        _SETUP,
        [
            "savepoint s",
            "update rp018 set b = 0 where a = 1",
            "rollback to s",
            "update rp018 set b = 0 where a = 2",
            "delete from rp018 where a = 3",
            "rollback to s",
            _ROWS,
            "update rp018 set b = 5 where a = 5",
            "release s",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "a unique column's values swapped": (
        _SETUP,
        [
            "savepoint s",
            "update rp018 set u = 100 where a = 1",
            "update rp018 set u = 1 where a = 2",
            "update rp018 set u = 2 where a = 1",
            _ROWS,
            "rollback to s",
            _ROWS,
            "select a from rp018 where u = 1",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "several tables": (
        [
            *_SETUP,
            "create table rp018_o (k int primary key, v text)",
            "insert into rp018_o values (1, 'x')",
        ],
        [
            "savepoint s",
            "update rp018 set b = 0",
            "insert into rp018_o values (2, 'y')",
            "update rp018_o set v = 'z' where k = 1",
            "rollback to s",
            _ROWS,
            "select * from rp018_o order by k",
            "commit",
        ],
        ["select * from rp018_o order by k", "drop table rp018_o", *_CLEANUP],
    ),
    "a failed statement, then ROLLBACK TO": (
        _SETUP,
        [
            "savepoint s",
            "update rp018 set b = 0 where a = 1",
            "insert into rp018 values (2, 0, 99, 'dup')",
            "rollback to s",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "CREATE TABLE and CREATE INDEX undone": (
        _SETUP,
        [
            "savepoint s",
            "create table rp018_new (k int primary key)",
            "insert into rp018_new values (1)",
            "create index rp018_bt on rp018 (t)",
            "rollback to s",
            "select count(*) from pg_class where relname = 'rp018_new'",
            "select indexname from pg_indexes where tablename = 'rp018' order by 1",
            "create table rp018_new (k text)",
            "insert into rp018_new values ('again')",
            "select * from rp018_new",
            "commit",
        ],
        ["select * from rp018_new", "drop table rp018_new", *_CLEANUP],
    ),
    "DROP TABLE undone, rows and indexes": (
        _SETUP,
        [
            "update rp018 set t = 'in block' where a = 1",
            "savepoint s",
            "delete from rp018 where a = 2",
            "drop table rp018",
            "rollback to s",
            _ROWS,
            _BY_B,
            "select indexname from pg_indexes where tablename = 'rp018' order by 1",
            "savepoint t",
            "insert into rp018 values (9, 0, 1, 'dup u')",
            "rollback to t",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "ALTER TABLE ADD COLUMN with a default undone": (
        _SETUP,
        [
            "savepoint s",
            "alter table rp018 add column c int default 7",
            "update rp018 set c = 8 where a = 1",
            "rollback to s",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "ALTER COLUMN TYPE undone": (
        _SETUP,
        [
            "savepoint s",
            "alter table rp018 alter column b type text",
            "rollback to s",
            _ROWS,
            "select pg_typeof(b)::text from rp018 limit 1",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "TRUNCATE undone": (
        _SETUP,
        [
            "savepoint s",
            "truncate rp018",
            "select count(*) from rp018",
            "insert into rp018 values (1, 1, 1, 'after truncate')",
            "rollback to s",
            _ROWS,
            "savepoint t",
            "insert into rp018 values (9, 0, 1, 'dup u')",
            "rollback to t",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "a trigger's writes to another table undone": (
        [
            *_SETUP,
            "create table rp018_audit (n serial, what text)",
            "create function rp018_log() returns trigger language plpgsql as $$ "
            "begin insert into rp018_audit (what) values (tg_op || ' ' || new.a); "
            "return new; end $$",
            "create trigger rp018_t after insert or update on rp018 "
            "for each row execute function rp018_log()",
        ],
        [
            "update rp018 set b = 11 where a = 1",
            "savepoint s",
            "update rp018 set b = 22 where a = 2",
            "insert into rp018 values (8, 80, 8, 'r8')",
            "select what from rp018_audit order by n",
            "rollback to s",
            "select what from rp018_audit order by n",
            _ROWS,
            "insert into rp018 values (9, 90, 9, 'r9')",
            "select n, what from rp018_audit order by n",
            "commit",
        ],
        [
            "select n, what from rp018_audit order by n",
            "drop table rp018_audit",
            *_CLEANUP,
            "drop function rp018_log",
        ],
    ),
    "a PL/pgSQL EXCEPTION block undoes its writes": (
        _SETUP,
        [
            "update rp018 set t = 'outside' where a = 4",
            "do $$ begin "
            "  update rp018 set b = 0 where a = 1; "
            "  begin "
            "    update rp018 set b = -1 where a = 2; "
            "    delete from rp018 where a = 3; "
            "    insert into rp018 values (1, 0, 0, 'dup'); "
            "  exception when unique_violation then "
            "    update rp018 set t = 'handled' where a = 5; "
            "  end; "
            "end $$",
            _ROWS,
            "savepoint s",
            "do $$ begin "
            "  begin "
            "    update rp018 set b = -9 where a = 5; "
            "    perform 1 / 0; "
            "  exception when division_by_zero then null; "
            "  end; "
            "  update rp018 set t = 'after' where a = 1; "
            "end $$",
            _ROWS,
            "rollback to s",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "INSERT ON CONFLICT undone": (
        _SETUP,
        [
            "savepoint s",
            "insert into rp018 values (1, 0, 1, 'c'), (6, 60, 6, 'r6') "
            "on conflict (a) do update set b = excluded.b, t = 'conflict'",
            "insert into rp018 values (2, 0, 2, 'n') on conflict do nothing",
            _ROWS,
            "rollback to s",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "MERGE undone": (
        _SETUP,
        [
            "savepoint s",
            "merge into rp018 using (values (1, 100), (9, 900)) v(a, b) on rp018.a = v.a "
            "when matched then update set b = v.b "
            "when not matched then insert values (v.a, v.b, v.a, 'm')",
            "rollback to s",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "COPY undone": (
        _SETUP,
        [
            "savepoint s",
            "%copy rp018|10\t100\t10\tc10\n11\t110\t11\tc11\n",
            "select count(*) from rp018",
            "rollback to s",
            _ROWS,
            "%copy rp018|12\t120\t12\tc12\n",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "a sequence is not rolled back, a CREATE SEQUENCE is": (
        [
            "create table rp018_s (id serial primary key, v text)",
            "insert into rp018_s (v) values ('one')",
        ],
        [
            "savepoint s",
            "insert into rp018_s (v) values ('two')",
            "create sequence rp018_q",
            "select nextval('rp018_q')",
            "rollback to s",
            "select currval('rp018_s_id_seq')",
            "insert into rp018_s (v) values ('three')",
            "select * from rp018_s order by id",
            "!select nextval('rp018_q')",
            "rollback",
        ],
        [
            "select * from rp018_s order by id",
            "select last_value from rp018_s_id_seq",
            "drop table rp018_s",
        ],
    ),
    "a large object made or written after the savepoint": (
        [],
        [
            "select lo_create(424242)",
            "select lo_put(424242, 0, 'before')",
            "savepoint s",
            "select lo_put(424242, 0, 'AFTER!')",
            "select lo_create(424243)",
            "rollback to s",
            "select convert_from(lo_get(424242), 'UTF8')",
            "select count(*) from pg_largeobject_metadata where oid in (424242, 424243)",
            "rollback",
        ],
        [],
    ),
    "a comment and a rename undone": (
        _SETUP,
        [
            "savepoint s",
            "comment on table rp018 is 'commented'",
            "alter table rp018 rename to rp018_renamed",
            "rollback to s",
            "select obj_description('rp018'::regclass, 'pg_class')",
            "select count(*) from pg_class where relname = 'rp018_renamed'",
            _ROWS,
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "ctid, xmin and size after ROLLBACK TO": (
        _SETUP,
        [
            "savepoint s",
            "update rp018 set b = 0 where a = 1",
            "insert into rp018 values (6, 60, 6, 'r6')",
            "rollback to s",
            "select a, ctid from rp018 order by a",
            "select count(distinct xmin::text) from rp018",
            "select pg_relation_size('rp018')",
            "update rp018 set b = 1 where a = 1",
            "select ctid from rp018 where a = 1",
            "select n_tup_ins, n_tup_upd, n_tup_del from pg_stat_xact_user_tables "
            "where relname = 'rp018'",
            "commit",
        ],
        [*_CLEANUP],
    ),
    "a GIN-indexed column": (
        [
            "create table rp018_g (k int primary key, d tsvector)",
            "create index rp018_gi on rp018_g using gin (d)",
            "insert into rp018_g values (1, 'cat dog'), (2, 'cow')",
        ],
        [
            "savepoint s",
            "update rp018_g set d = 'bird' where k = 1",
            "insert into rp018_g values (3, 'cat')",
            "rollback to s",
            "select k from rp018_g where d @@ 'cat'::tsquery order by k",
            "select k from rp018_g where d @@ 'bird'::tsquery order by k",
            "commit",
        ],
        ["drop table rp018_g"],
    ),
    "SERIALIZABLE block with a ROLLBACK TO commits": (
        _SETUP,
        [
            "savepoint s",
            "update rp018 set b = 0 where a = 1",
            "rollback to s",
            "update rp018 set b = 1 where a = 2",
            "commit",
        ],
        [_ROWS, *_CLEANUP],
    ),
    "REPEATABLE READ after another session commits": (
        [*_SETUP, "create table rp018_other (x int)"],
        [
            _ROWS,
            "update rp018 set t = 'block' where a = 1",
            "savepoint s",
            "@b insert into rp018_other values (1)",
            "update rp018 set b = 0 where a = 2",
            "delete from rp018 where a = 3",
            "insert into rp018 values (6, 60, 6, 'r6')",
            "rollback to s",
            _ROWS,
            "select count(*) from rp018_other",
            "commit",
        ],
        [_ROWS, "drop table rp018_other", *_CLEANUP],
    ),
}

_BEGIN = {
    "SERIALIZABLE block with a ROLLBACK TO commits": "begin isolation level serializable",
    "REPEATABLE READ after another session commits": "begin isolation level repeatable read",
}


def statements(name: str, forced: bool) -> list[str]:
    """The scenario's statements; `forced` makes the block drop a table
    first, so that no ROLLBACK TO in it can be a move."""
    before, block, after = SCENARIOS[name]
    begin = _BEGIN.get(name, "begin")
    if not forced:
        return [*before, begin, *block, *after]
    return [
        *before,
        _FORCE_SETUP,
        begin,
        _FORCE,
        *block,
        *after,
        "drop table if exists rp018_force",
    ]


def _copy(conn: Any, spec: str) -> Any:
    table, rows = spec.split("|", 1)
    try:
        with conn.cursor() as cur:
            with cur.copy(f"copy {table} from stdin") as copy:
                copy.write(rows)
            return cur.statusmessage
    except psycopg.Error as e:
        return ("ERR", e.sqlstate, e.diag.message_primary)


def run(dsn: str, stmts: list[str]) -> list[Any]:
    conns: dict[str, Any] = {}

    def conn(name: str) -> Any:
        if name not in conns:
            conns[name] = psycopg.connect(dsn, autocommit=True)
        return conns[name]

    out: list[Any] = []
    try:
        for s in stmts:
            name = "a"
            if s.startswith("@"):
                name, s = s[1:].split(" ", 1)
            if s.startswith("%copy "):
                out.append(_copy(conn(name), s[len("%copy ") :]))
            elif s.startswith("!"):
                out.append(_answer(conn(name), s[1:], detail=True))
            else:
                out.append(_answer(conn(name), s))
    finally:
        for c in conns.values():
            if c.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
                c.execute("rollback")
            c.close()
    return out


def expected(name: str, forced: bool) -> list[Any]:
    """15.19's answers, with the forcing statements' own answers put in."""
    got = list(EXPECTED[name])
    if not forced:
        return got
    before, _, after = SCENARIOS[name]
    # create rp018_force, BEGIN, the DROP ... then the trailing drop.
    nb = len(before)
    got[nb:nb] = ["CREATE TABLE"]
    got.insert(nb + 2, "DROP TABLE")
    got.append("DROP TABLE")
    return got


# Cases another item holds open: their gap tests until it closes.
_OPEN_ELSEWHERE = {
    # Putting back two rows that swapped a unique value deletes and inserts
    # them again, which the old value's claim refuses (RP-296).
    ("a unique column's values swapped", True): "RP-296",
}


def _cases() -> list[Any]:
    out = []
    for name in sorted(SCENARIOS):
        for forced, label in ((False, "move"), (True, "undo-journal")):
            item = _OPEN_ELSEWHERE.get((name, forced))
            mark = pytest.mark.backlog(item) if item else pytest.mark.backlog_fixed("RP-018")
            out.append(pytest.param(name, forced, marks=mark, id=f"{name}-{label}"))
    return out


@pytest.mark.parametrize(("name", "forced"), _cases())
def test_savepoint_scenario(name: str, forced: bool, rust_pg: Any) -> None:
    got = run(_dsn(rust_pg) + " dbname=postgres", statements(name, forced))
    assert got == expected(name, forced)


def _fill(c: Any, table: str, n: int) -> None:
    c.execute(f"create table {table} (a int primary key, b int)")
    c.execute(f"insert into {table} select g, g from generate_series(1, {n}) g")


@pytest.mark.backlog_fixed("RP-018")
def test_savepoint_cost_is_the_rows_written_not_the_table(rust_pg: Any) -> None:
    """SAVEPOINT, a one-row write and ROLLBACK TO cost about the same over
    a 200,000-row table as over a 20-row one, by a move and by the undo
    journal alike.

    The first write after a savepoint used to copy the whole table into it,
    and a ROLLBACK TO that could not move the block rewrote the table from
    that copy: on a debug build the 200,000-row round took 0.2 s by a move
    and 1.9 s without one, against 0.01 s for 20 rows. It is a one-row
    INSERT, not an UPDATE, so that the planner's own pricing of a scan
    (RP-297, after the forcing DROP) does not swamp what is measured. The budget is generous for a
    slow CI runner."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        _fill(c, "rp018_big", 200_000)
        _fill(c, "rp018_small", 20)

        def round_trip(table: str, forced: bool) -> float:
            c.execute("create table if not exists rp018_force (x int)")
            c.execute("begin")
            if forced:
                c.execute("drop table rp018_force")
            start = time.perf_counter()
            c.execute("savepoint s")
            c.execute(f"insert into {table} values (-1, -1)")
            c.execute("rollback to s")
            spent = time.perf_counter() - start
            assert c.execute(f"select count(*) from {table} where a = -1").fetchone() == (0,)
            c.execute("commit")
            return spent

        for forced in (False, True):
            small = min(round_trip("rp018_small", forced) for _ in range(3))
            big = min(round_trip("rp018_big", forced) for _ in range(3))
            assert big < 4 * small + 0.1, (forced, small, big)
        for t in ("rp018_big", "rp018_small", "rp018_force"):
            c.execute(f"drop table if exists {t}")


@pytest.mark.backlog("RP-296")
def test_an_updated_unique_value_can_be_used_again(rust_pg: Any) -> None:
    """After an UPDATE moves a row off a unique value, another row may take
    it: PostgreSQL 15.19 inserts it. Here the storage's claim on the old
    value stays with the row, so the insert failed `XX000 could not
    insert: E11000 duplicate key error on index rp018_uq_u_key`, in a block
    or out of one."""
    got = run(
        _dsn(rust_pg) + " dbname=postgres",
        [
            "create table rp018_uq (a int primary key, u int unique)",
            "insert into rp018_uq values (1, 1)",
            "update rp018_uq set u = 100 where a = 1",
            "insert into rp018_uq values (2, 1)",
            "select * from rp018_uq order by a",
            "drop table rp018_uq",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "INSERT 0 1",
        "UPDATE 1",
        "INSERT 0 1",
        [("1", "100"), ("2", "1")],
        "DROP TABLE",
    ]


@pytest.mark.backlog("RP-297")
def test_a_key_lookup_after_ddl_does_not_read_the_table(rust_pg: Any) -> None:
    """A primary-key SELECT costs about the same over 200,000 rows as over
    20, as PostgreSQL's index scan does, whatever DDL ran before it. Here
    the planner prices the scan from the table's heap page count, which
    `heap_spaces` computes by decoding every row (`table_docs`), and the
    count is kept only until the catalog moves: after any CREATE or DROP,
    of any table, the next lookup took about 2.1 s over 200,000 rows on a
    debug build (2026-10-26), against milliseconds over 20."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        _fill(c, "rp297_big", 200_000)
        _fill(c, "rp297_small", 20)

        def lookup(table: str) -> float:
            c.execute("create table rp297_scratch (x int)")
            c.execute("drop table rp297_scratch")
            start = time.perf_counter()
            assert c.execute(f"select b from {table} where a = 1").fetchone() == (1,)
            return time.perf_counter() - start

        small = min(lookup("rp297_small") for _ in range(3))
        big = min(lookup("rp297_big") for _ in range(3))
        c.execute("drop table rp297_big")
        c.execute("drop table rp297_small")
        assert big < 4 * small + 0.1, (small, big)


EXPECTED: dict[str, Any] = {
    "update, insert and delete undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 2",
        "INSERT 0 1",
        "DELETE 1",
        [
            ("1", "11", "1", "r1"),
            ("2", "21", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("6", "60", "6", "r6"),
        ],
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        [("2",)],
        [("1",)],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "one row written many times": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        "UPDATE 1",
        "DELETE 1",
        "INSERT 0 1",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "nested savepoints rolled back inner then outer": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "UPDATE 1",
        "SAVEPOINT",
        "UPDATE 1",
        "SAVEPOINT",
        "UPDATE 2",
        "INSERT 0 1",
        "ROLLBACK",
        [
            ("1", "-1", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "before"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "UPDATE 1",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "before"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "before"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "RELEASE then the outer ROLLBACK TO undoes both": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        "SAVEPOINT",
        "UPDATE 1",
        "DELETE 1",
        "RELEASE",
        [
            ("2", "0", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "ROLLBACK TO, more writes, ROLLBACK TO again": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        "ROLLBACK",
        "UPDATE 1",
        "DELETE 1",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "UPDATE 1",
        "RELEASE",
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "5", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "a unique column's values swapped": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        "UPDATE 1",
        "UPDATE 1",
        [
            ("1", "10", "2", "r1"),
            ("2", "20", "1", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        [("1",)],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "several tables": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "CREATE TABLE",
        "INSERT 0 1",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 5",
        "INSERT 0 1",
        "UPDATE 1",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        [("1", "x")],
        "COMMIT",
        [("1", "x")],
        "DROP TABLE",
        "DROP TABLE",
    ],
    "a failed statement, then ROLLBACK TO": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        ("ERR", "23505", 'duplicate key value violates unique constraint "rp018_pkey"', None),
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "CREATE TABLE and CREATE INDEX undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "CREATE TABLE",
        "INSERT 0 1",
        "CREATE INDEX",
        "ROLLBACK",
        [("0",)],
        [("rp018_b",), ("rp018_pkey",), ("rp018_u_key",)],
        "CREATE TABLE",
        "INSERT 0 1",
        [("again",)],
        "COMMIT",
        [("again",)],
        "DROP TABLE",
        "DROP TABLE",
    ],
    "DROP TABLE undone, rows and indexes": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "UPDATE 1",
        "SAVEPOINT",
        "DELETE 1",
        "DROP TABLE",
        "ROLLBACK",
        [
            ("1", "10", "1", "in block"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        [("2",)],
        [("rp018_b",), ("rp018_pkey",), ("rp018_u_key",)],
        "SAVEPOINT",
        ("ERR", "23505", 'duplicate key value violates unique constraint "rp018_u_key"', None),
        "ROLLBACK",
        "COMMIT",
        [
            ("1", "10", "1", "in block"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "ALTER TABLE ADD COLUMN with a default undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "ALTER TABLE",
        "UPDATE 1",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "ALTER COLUMN TYPE undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "ALTER TABLE",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        [("integer",)],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "TRUNCATE undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "TRUNCATE TABLE",
        [("0",)],
        "INSERT 0 1",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "SAVEPOINT",
        ("ERR", "23505", 'duplicate key value violates unique constraint "rp018_u_key"', None),
        "ROLLBACK",
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "a trigger's writes to another table undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "CREATE TABLE",
        "CREATE FUNCTION",
        "CREATE TRIGGER",
        "BEGIN",
        "UPDATE 1",
        "SAVEPOINT",
        "UPDATE 1",
        "INSERT 0 1",
        [("UPDATE 1",), ("UPDATE 2",), ("INSERT 8",)],
        "ROLLBACK",
        [("UPDATE 1",)],
        [
            ("1", "11", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "INSERT 0 1",
        [("1", "UPDATE 1"), ("4", "INSERT 9")],
        "COMMIT",
        [("1", "UPDATE 1"), ("4", "INSERT 9")],
        "DROP TABLE",
        "DROP TABLE",
        "DROP FUNCTION",
    ],
    "a PL/pgSQL EXCEPTION block undoes its writes": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "UPDATE 1",
        "DO",
        [
            ("1", "0", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "outside"),
            ("5", "50", "5", "handled"),
        ],
        "SAVEPOINT",
        "DO",
        [
            ("1", "0", "1", "after"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "outside"),
            ("5", "50", "5", "handled"),
        ],
        "ROLLBACK",
        [
            ("1", "0", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "outside"),
            ("5", "50", "5", "handled"),
        ],
        "COMMIT",
        [
            ("1", "0", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "outside"),
            ("5", "50", "5", "handled"),
        ],
        "DROP TABLE",
    ],
    "INSERT ON CONFLICT undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "INSERT 0 2",
        "INSERT 0 0",
        [
            ("1", "0", "1", "conflict"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
            ("6", "60", "6", "r6"),
        ],
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "MERGE undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "MERGE 2",
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "COPY undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "COPY 2",
        [("7",)],
        "ROLLBACK",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COPY 1",
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
            ("12", "120", "12", "c12"),
        ],
        "DROP TABLE",
    ],
    "a sequence is not rolled back, a CREATE SEQUENCE is": [
        "CREATE TABLE",
        "INSERT 0 1",
        "BEGIN",
        "SAVEPOINT",
        "INSERT 0 1",
        "CREATE SEQUENCE",
        [("1",)],
        "ROLLBACK",
        [("2",)],
        "INSERT 0 1",
        [("1", "one"), ("3", "three")],
        ("ERR", "42P01", 'relation "rp018_q" does not exist', None, None),
        "ROLLBACK",
        [("1", "one")],
        [("3",)],
        "DROP TABLE",
    ],
    "a large object made or written after the savepoint": [
        "BEGIN",
        [("424242",)],
        [("",)],
        "SAVEPOINT",
        [("",)],
        [("424243",)],
        "ROLLBACK",
        [("before",)],
        [("1",)],
        "ROLLBACK",
    ],
    "a comment and a rename undone": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "COMMENT",
        "ALTER TABLE",
        "ROLLBACK",
        [("None",)],
        [("0",)],
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "ctid, xmin and size after ROLLBACK TO": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        "INSERT 0 1",
        "ROLLBACK",
        [("1", "(0,1)"), ("2", "(0,2)"), ("3", "(0,3)"), ("4", "(0,4)"), ("5", "(0,5)")],
        [("1",)],
        [("8192",)],
        "UPDATE 1",
        [("(0,8)",)],
        [("6", "2", "0")],
        "COMMIT",
        "DROP TABLE",
    ],
    "a GIN-indexed column": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 2",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        "INSERT 0 1",
        "ROLLBACK",
        [("1",)],
        [],
        "COMMIT",
        "DROP TABLE",
    ],
    "SERIALIZABLE block with a ROLLBACK TO commits": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "BEGIN",
        "SAVEPOINT",
        "UPDATE 1",
        "ROLLBACK",
        "UPDATE 1",
        "COMMIT",
        [
            ("1", "10", "1", "r1"),
            ("2", "1", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
    ],
    "REPEATABLE READ after another session commits": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 5",
        "CREATE TABLE",
        "BEGIN",
        [
            ("1", "10", "1", "r1"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "UPDATE 1",
        "SAVEPOINT",
        "INSERT 0 1",
        "UPDATE 1",
        "DELETE 1",
        "INSERT 0 1",
        "ROLLBACK",
        [
            ("1", "10", "1", "block"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        [("0",)],
        "COMMIT",
        [
            ("1", "10", "1", "block"),
            ("2", "20", "2", "r2"),
            ("3", "30", "3", "r3"),
            ("4", "40", "4", "r4"),
            ("5", "50", "5", "r5"),
        ],
        "DROP TABLE",
        "DROP TABLE",
    ],
}

if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run(dsn, statements(name, False)) for name in SCENARIOS})
