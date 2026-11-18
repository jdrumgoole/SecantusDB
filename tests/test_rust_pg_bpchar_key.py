"""A `bpchar` PRIMARY KEY on the Rust PostgreSQL server: keyed by the value
with its trailing blanks trimmed, while the row keeps the value as written
(RP-693, step 1).

PostgreSQL compares `bpchar` with `bpchareq` / `bcTruelen`, which ignore
trailing blanks, so `'k'` and `'k  '` are the same key; but an unsized
`bpchar` column keeps the value as written, and `octet_length` sees the
blanks. This server stored the value in `_id` with the blanks trimmed, so the
key was right and the value lost its blanks. A table created now keeps the
value under the column's own field and derives `_id` from it, trimmed (Joe,
2026-11-18). A table created before keeps the old layout and its answers.

COPY wrote a `bpchar` key untrimmed into `_id`, so a later `'w'` did not
collide with a copied `'w  '`, for a `char(n)`, a composite and an unsized
key alike; every write now trims the key.

Every expected answer was measured on PostgreSQL 15.19 (2026-11-18).

Skipped unless `secantusd-pg` has been built; CI's `pg-oracle` lane builds it
and names this file.
"""

from __future__ import annotations

import re
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest

from tests.server_pipes import drain

psycopg = pytest.importorskip("psycopg")

REPO = Path(__file__).resolve().parents[1]
BINARY = (
    REPO
    / "crates"
    / "secantus-pg"
    / "target"
    / "debug"
    / ("secantusd-pg.exe" if sys.platform == "win32" else "secantusd-pg")
)

pytestmark = pytest.mark.skipif(
    not BINARY.exists(),
    reason=f"{BINARY.relative_to(REPO)} not built (cargo build in crates/secantus-pg)",
)


class _Server:
    def __init__(self, home: Path) -> None:
        self.home = home
        self.proc: subprocess.Popen[str] | None = None
        self.port = ""

    def start(self) -> None:
        self.proc = subprocess.Popen(
            [str(BINARY), str(self.home), "127.0.0.1:0", "--trust-unknown-users"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        line = self.proc.stdout.readline() if self.proc.stdout else ""
        match = re.search(r"listening on \S+:(\d+)", line)
        assert match, f"secantusd-pg did not start: {line!r}"
        drain(self.proc)
        self.port = match.group(1)

    def connect(self) -> psycopg.Connection:
        return psycopg.connect(
            f"host=127.0.0.1 port={self.port} dbname=postgres user=probe",
            autocommit=True,
            connect_timeout=10,
        )

    def stop(self) -> None:
        if self.proc is None:
            return
        self.proc.terminate()
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=30)
        self.proc = None


@pytest.fixture
def server(tmp_path: Path) -> Iterator[_Server]:
    s = _Server(tmp_path / "home")
    s.start()
    try:
        yield s
    finally:
        s.stop()


@pytest.fixture
def connection(server: _Server) -> Iterator[psycopg.Connection]:
    with server.connect() as conn:
        yield conn


def _rows(c: psycopg.Connection, q: str, params: tuple[object, ...] | None = None) -> list:
    return [tuple(r) for r in c.execute(q, params).fetchall()]


def _copy(c: psycopg.Connection, table: str, text: str) -> None:
    with c.cursor().copy(f"copy {table} from stdin") as cp:
        cp.write(text)


def test_a_bpchar_primary_key_keeps_the_value_as_written(
    connection: psycopg.Connection,
) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    c.execute("insert into p values ('k  ', 1), ('m', 2)")
    assert _rows(c, "select a, octet_length(a), b from p order by b") == [
        ("k  ", 3, 1),
        ("m", 1, 2),
    ]
    # The key ignores the blanks, both ways round.
    for dup in ("'k'", "'m '"):
        with pytest.raises(psycopg.errors.UniqueViolation):
            c.execute(f"insert into p values ({dup}, 9)")
    with pytest.raises(psycopg.errors.UniqueViolation):
        c.execute("insert into p select 'k ', 9")


def test_equality_finds_the_row_whatever_its_blanks(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    c.execute("insert into p values ('k  ', 1), ('m', 2)")
    for where in (
        "a = 'k'",
        "a = 'k '::bpchar",
        "a in ('k', 'zz')",
        "a = 'k'::text",
        "a::text = 'k'",
    ):
        assert _rows(c, f"select octet_length(a), b from p where {where}") == [(3, 1)], where
    assert _rows(c, "select octet_length(a), b from p where a > 'k'") == [(1, 2)]
    # A parameter, prepared (the extended protocol's point lookup).
    for _ in range(2):
        assert _rows(c, "select octet_length(a), b from p where a = %s", ("k",)) == [(3, 1)]
    assert _rows(c, "select b from p where a = %s::bpchar", ("m   ",)) == [(2,)]


def test_on_conflict_arbitrates_on_the_trimmed_key(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    c.execute("insert into p values ('k  ', 1)")
    assert _rows(
        c,
        "insert into p values ('k ', 5) on conflict (a) do update set b = excluded.b "
        "returning octet_length(a), b",
    ) == [(3, 5)]
    assert _rows(c, "insert into p values ('k', 6) on conflict do nothing returning b") == []
    assert _rows(c, "select a, b from p") == [("k  ", 5)]


def test_update_and_delete_move_the_key(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    c.execute("insert into p values ('k  ', 1), ('m', 2)")
    assert _rows(c, "update p set b = 7 where a = 'k' returning octet_length(a), b") == [(3, 7)]
    assert _rows(c, "update p set a = 'q   ' where b = 2 returning octet_length(a), b") == [(4, 2)]
    assert _rows(c, "select octet_length(a), b from p where a = 'q'") == [(4, 2)]
    with pytest.raises(psycopg.errors.UniqueViolation):
        c.execute("insert into p values ('q', 0)")
    # The old key is free again.
    c.execute("insert into p values ('m', 3)")
    assert _rows(c, "delete from p where a = 'k   ' returning octet_length(a)") == [(3,)]
    assert _rows(c, "select a, b from p order by b") == [("q   ", 2), ("m", 3)]


def test_a_before_trigger_rewriting_the_key(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    c.execute(
        "create function pad() returns trigger language plpgsql as "
        "$$ begin new.a := new.a || '  '; return new; end $$"
    )
    c.execute("create trigger tg before insert or update on p for each row execute function pad()")
    assert _rows(c, "insert into p values ('t', 6) returning octet_length(a)") == [(3,)]
    with pytest.raises(psycopg.errors.UniqueViolation):
        c.execute("insert into p values ('t', 7)")
    assert _rows(c, "update p set b = 8 where a = 't' returning octet_length(a), b") == [(3, 8)]


def test_foreign_keys_match_the_trimmed_key(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    c.execute("insert into p values ('x  ', 1), ('z ', 3)")
    c.execute("create table ch (r char(3) references p on delete cascade on update cascade, n int)")
    c.execute("insert into ch values ('x', 10), ('z', 30)")
    with pytest.raises(psycopg.errors.ForeignKeyViolation):
        c.execute("insert into ch values ('zz', 0)")
    assert _rows(c, "update p set a = 'w ' where b = 3 returning octet_length(a)") == [(2,)]
    assert _rows(c, "select r, n from ch order by n") == [("x  ", 10), ("w  ", 30)]
    assert _rows(c, "delete from p where a = 'x' returning octet_length(a)") == [(3,)]
    assert _rows(c, "select r, n from ch order by n") == [("w  ", 30)]


def test_copy_keeps_the_value_and_trims_the_key(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    _copy(c, "p", "w  \t11\n")
    assert _rows(c, "select octet_length(a), b from p") == [(3, 11)]
    with pytest.raises(psycopg.errors.UniqueViolation):
        c.execute("insert into p values ('w', 2)")


@pytest.mark.parametrize(
    "ddl, row, dup",
    [
        # A key added by ALTER TABLE keeps the trimmed layout.
        (
            "create table p (a bpchar, b int); alter table p add primary key (a)",
            "w  \t1\n",
            "('w', 2)",
        ),
        ("create table p (a char(3) primary key, b int)", "w \t1\n", "('w', 2)"),
        (
            "create table p (a bpchar, b char(2), primary key (a, b))",
            "w  \tb \n",
            "('w', 'b')",
        ),
    ],
    ids=["altered-unsized", "char3", "composite"],
)
def test_copy_trims_every_bpchar_key(
    connection: psycopg.Connection, ddl: str, row: str, dup: str
) -> None:
    c = connection
    for q in ddl.split("; "):
        c.execute(q)
    _copy(c, "p", row)
    with pytest.raises(psycopg.errors.UniqueViolation):
        c.execute(f"insert into p values {dup}")
    assert _rows(c, "select count(*) from p") == [(1,)]


def test_dropping_the_key_keeps_the_values(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table p (a bpchar primary key, b int)")
    c.execute("insert into p values ('v   ', 1), ('y', 2)")
    c.execute("alter table p drop constraint p_pkey")
    assert _rows(c, "select a, octet_length(a), b from p order by b") == [
        ("v   ", 4, 1),
        ("y", 1, 2),
    ]
    c.execute("insert into p values ('y', 9)")
    assert _rows(c, "select count(*) from p where a = 'y'") == [(2,)]


def test_the_layout_survives_a_restart(server: _Server) -> None:
    with server.connect() as c:
        c.execute("create table p (a bpchar primary key, b int)")
        c.execute("insert into p values ('x  ', 1)")
    server.stop()
    server.start()
    with server.connect() as c:
        assert _rows(c, "select a, octet_length(a) from p where a = 'x'") == [("x  ", 3)]
        with pytest.raises(psycopg.errors.UniqueViolation):
            c.execute("insert into p values ('x', 2)")


# Step 2 (RP-693): a UNIQUE over an unsized `bpchar` is a storage unique index
# over a hidden field holding the value with its trailing blanks trimmed --
# PostgreSQL's btree over `bpcharcmp` (Joe, 2026-11-18). Measured on 15.19.


def test_a_bpchar_unique_constraint_ignores_trailing_blanks(
    connection: psycopg.Connection,
) -> None:
    c = connection
    c.execute("create table u1 (a bpchar unique, b int)")
    c.execute("insert into u1 values ('k  ', 1)")
    with pytest.raises(psycopg.errors.UniqueViolation) as e:
        c.execute("insert into u1 values ('k', 2)")
    assert e.value.diag.constraint_name == "u1_a_key"
    assert e.value.diag.message_detail == "Key (a)=(k) already exists."
    # NULLs stay distinct.
    c.execute("insert into u1 values (null, 3), (null, 4)")
    assert _rows(c, "select indexdef from pg_indexes where tablename = 'u1'") == [
        ("CREATE UNIQUE INDEX u1_a_key ON public.u1 USING btree (a)",)
    ]


def test_on_conflict_on_a_bpchar_unique_arbitrates_on_the_trimmed_key(
    connection: psycopg.Connection,
) -> None:
    c = connection
    c.execute("create table u1 (a bpchar unique, b int)")
    c.execute("insert into u1 values ('k  ', 1)")
    c.execute("insert into u1 values ('k ', 3) on conflict (a) do update set b = excluded.b")
    # PostgreSQL's row keeps `octet_length(a)` 3: the write keeps the blanks
    # only at step 5.
    assert _rows(c, "select b from u1") == [(3,)]
    assert _rows(c, "insert into u1 values ('k', 6) on conflict do nothing returning b") == []


def test_a_bpchar_unique_index_and_a_composite_unique(connection: psycopg.Connection) -> None:
    c = connection
    c.execute("create table u2 (a bpchar, b int)")
    c.execute("create unique index u2i on u2 (a)")
    c.execute("insert into u2 values ('k', 1)")
    with pytest.raises(psycopg.errors.UniqueViolation) as e:
        c.execute("insert into u2 values ('k  ', 2)")
    assert e.value.diag.constraint_name == "u2i"
    c.execute("create table u3 (a bpchar, c int, unique (a, c))")
    c.execute("insert into u3 values ('z', 1), ('z ', 2)")
    with pytest.raises(psycopg.errors.UniqueViolation) as e:
        c.execute("insert into u3 values ('z ', 1)")
    assert e.value.diag.constraint_name == "u3_a_c_key"
    assert _rows(
        c, "select indexdef from pg_indexes where tablename in ('u2', 'u3') order by 1"
    ) == [
        ("CREATE UNIQUE INDEX u2i ON public.u2 USING btree (a)",),
        ("CREATE UNIQUE INDEX u3_a_c_key ON public.u3 USING btree (a, c)",),
    ]


def test_copy_and_an_added_unique_collide_on_the_trimmed_key(server: _Server) -> None:
    with server.connect() as c:
        c.execute("create table u (a bpchar unique, b int)")
        _copy(c, "u", "w  \t1\n")
        with pytest.raises(psycopg.errors.UniqueViolation):
            c.execute("insert into u values ('w', 2)")
        c.execute("create table v (a bpchar, b int)")
        c.execute("insert into v values ('q', 1), ('r', 2)")
        c.execute("alter table v add unique (a)")
        with pytest.raises(psycopg.errors.UniqueViolation):
            c.execute("insert into v values ('q ', 3)")
    server.stop()
    server.start()
    with server.connect() as c:
        with pytest.raises(psycopg.errors.UniqueViolation):
            c.execute("insert into u values ('w ', 3)")
        with pytest.raises(psycopg.errors.UniqueViolation):
            c.execute("insert into v values ('r  ', 3)")
        assert _rows(c, "select count(*) from u") == [(1,)]


@pytest.mark.parametrize("ddl", ["create unique index xi on x (a)", "alter table x add unique (a)"])
def test_a_bpchar_unique_over_rows_differing_only_in_blanks_is_refused(
    connection: psycopg.Connection, ddl: str
) -> None:
    # COPY keeps a non-key `bpchar` cell's blanks, so the rows differ only in
    # them; the build reports the row stored first, as written.
    c = connection
    c.execute("create table x (a bpchar, b int)")
    _copy(c, "x", "k  \t1\n")
    c.execute("insert into x values ('k', 2)")
    with pytest.raises(psycopg.errors.UniqueViolation) as e:
        c.execute(ddl)
    assert e.value.diag.message_primary.startswith("could not create unique index")
    assert e.value.diag.message_detail == "Key (a)=(k  ) is duplicated."
    assert _rows(c, "select count(*) from pg_indexes where tablename = 'x'") == [(0,)]
