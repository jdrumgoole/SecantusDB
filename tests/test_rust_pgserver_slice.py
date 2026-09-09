"""The Rust PostgreSQL server's P1 vertical slice, and its cross-server contract.

The headline assertion is not that the Rust server works on its own -- it is
that the Python server and the Rust server share one on-disk store. A catalog
document written subtly wrong by one is read as truth by the other, which is
silent data loss, so both directions are exercised here.

Skipped unless `secantusd-pg` has been built (it links WiredTiger and is
excluded from the clean workspace):

    cd crates/secantus-pgserver && cargo build
"""

from __future__ import annotations

import contextlib
import datetime as dt
import decimal as dc
import ipaddress
import shutil
import socket
import subprocess
import time
import uuid
from collections.abc import Iterator
from decimal import Decimal
from pathlib import Path

import pytest

psycopg = pytest.importorskip("psycopg")
from psycopg.types.multirange import Multirange  # noqa: E402
from psycopg.types.range import Range  # noqa: E402

REPO = Path(__file__).resolve().parents[1]
BINARY = REPO / "crates" / "secantus-pgserver" / "target" / "debug" / "secantusd-pg"

pytestmark = pytest.mark.skipif(
    not BINARY.exists(),
    reason=f"{BINARY.relative_to(REPO)} not built (cargo build in crates/secantus-pgserver)",
)


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


class _Server:
    """A `secantusd-pg` subprocess over one storage home."""

    def __init__(self, home: Path) -> None:
        self.home = home
        self.port = _free_port()
        self.proc: subprocess.Popen[str] | None = None

    def __enter__(self) -> _Server:
        # `_free_port()` reports a port the OS *had* free, but closes its probe
        # socket before the child binds -- so under `-n auto` a parallel worker
        # can claim the same port in the gap, and the child then exits with
        # "address already in use". That is the ONLY early exit worth retrying
        # (with a fresh port); any other early exit is a genuine startup crash
        # and must surface, not be masked. See the port-race note in CLAUDE.md.
        last_out = ""
        for _ in range(5):
            self.proc = subprocess.Popen(
                [str(BINARY), str(self.home), f"127.0.0.1:{self.port}"],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if self.proc.poll() is not None:
                    last_out = self.proc.stdout.read() if self.proc.stdout else ""
                    break
                try:
                    with socket.create_connection(("127.0.0.1", self.port), timeout=0.25):
                        return self
                except OSError:
                    time.sleep(0.05)
            else:
                raise RuntimeError("secantusd-pg did not start")
            if "address" in last_out.lower() and "use" in last_out.lower():
                self.port = _free_port()
                continue
            raise RuntimeError(f"secantusd-pg exited: {last_out}")
        raise RuntimeError(f"secantusd-pg could not bind a free port: {last_out}")

    def __exit__(self, *exc: object) -> None:
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)

    def connect(self, *, autocommit: bool = True) -> psycopg.Connection:
        return psycopg.connect(
            f"host=127.0.0.1 port={self.port} dbname=postgres user=test",
            autocommit=autocommit,
            connect_timeout=10,
        )


@pytest.fixture
def home(tmp_path: Path) -> Iterator[Path]:
    """A storage home the Rust server and the Python server both open.

    Only ONE may hold it at a time -- WiredTiger takes an exclusive lock -- so
    every test stops one before starting the other.
    """
    d = tmp_path / "pgstore"
    d.mkdir()
    yield d
    shutil.rmtree(d, ignore_errors=True)


def _python_sql(home: Path, *statements: str) -> list[tuple]:
    """Run statements through the PYTHON server over the same store."""
    from secantus.sql import run_sql
    from secantus.sql.session import Session
    from secantus.storage import Storage

    storage = Storage(str(home))
    try:
        session = Session()
        rows: list[tuple] = []
        for sql in statements:
            for result in run_sql(storage, "postgres", sql, session=session):
                rows.extend(result.rows)
        return rows
    finally:
        storage.close()


def test_create_insert_select_round_trip(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10),(2,'bob',20),(3,'carol',30)")
        cur.execute("SELECT id, name, n FROM t")
        assert sorted(cur.fetchall()) == [
            (1, "alice", 10),
            (2, "bob", 20),
            (3, "carol", 30),
        ]


@pytest.mark.parametrize(
    "where,expected",
    [
        ("id = 1", [1]),
        ("n > 15", [2, 3]),
        ("n >= 20 AND id <> 3", [2]),
        ("name = 'carol' OR n < 15", [1, 3]),
        ("n <= 20 AND (id = 1 OR name = 'bob')", [1, 2]),
    ],
)
def test_predicates_match_postgres(home: Path, where: str, expected: list[int]) -> None:
    """These answers were checked against a live PostgreSQL 14; PG is the oracle."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10),(2,'bob',20),(3,'carol',30)")
        cur.execute(f"SELECT id FROM t WHERE {where}")
        assert sorted(r[0] for r in cur.fetchall()) == expected


def test_the_python_server_reads_and_writes_a_rust_created_table(home: Path) -> None:
    """The catalog contract, in the direction that matters most.

    If the Rust server's catalog document diverges, the Python server does not
    fail loudly -- it sees a table with the wrong columns.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10),(2,'bob',20)")

    # Rust is stopped; Python opens the same store.
    assert _python_sql(home, "SELECT id, name, n FROM t ORDER BY id") == [
        (1, "alice", 10),
        (2, "bob", 20),
    ]
    _python_sql(home, "INSERT INTO t VALUES (3, 'carol', 30)")

    # ... and Rust sees what Python wrote.
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT id, name, n FROM t WHERE n > 15")
        assert sorted(cur.fetchall()) == [(2, "bob", 20), (3, "carol", 30)]


def test_the_rust_server_reads_a_python_created_table(home: Path) -> None:
    """The same contract in the other direction."""
    _python_sql(
        home,
        "CREATE TABLE py (k int PRIMARY KEY, label text)",
        "INSERT INTO py VALUES (9, 'made-by-python')",
    )
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT k, label FROM py WHERE k = 9")
        assert cur.fetchall() == [(9, "made-by-python")]


def test_duplicate_key_reports_what_postgres_reports(home: Path) -> None:
    """The storage layer speaks MongoDB (`E11000 duplicate key error ...`).
    None of that may reach a PostgreSQL client. Probed against PG 14."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        cur.execute("INSERT INTO t VALUES (1,'alice',10)")
        with pytest.raises(psycopg.errors.UniqueViolation) as exc:
            cur.execute("INSERT INTO t VALUES (1,'dup',1)")
    diag = exc.value.diag
    assert diag.sqlstate == "23505"
    assert diag.message_primary == ('duplicate key value violates unique constraint "t_pkey"')
    assert diag.message_detail == "Key (id)=(1) already exists."
    assert "E11000" not in str(exc.value)
    # The protocol's constraint fields, available since pgwire 0.39 and read by
    # pgjdbc via getServerErrorMessage().getConstraint(). `column_name` stays
    # unset because PostgreSQL leaves it unset on a 23505 -- it identifies the
    # column through the constraint (probed 14).
    assert diag.constraint_name == "t_pkey"
    assert diag.table_name == "t"
    assert diag.schema_name == "public"
    assert diag.column_name is None


@pytest.mark.parametrize(
    "sql,sqlstate",
    [
        ("SELECT nope FROM t", "42703"),
        ("SELECT * FROM t WHERE nope = 1", "42703"),
        ("SELECT * FROM missing", "42P01"),
        ("CREATE TABLE t (id int PRIMARY KEY)", "42P07"),
        # Unsupported must be an honest 0A000 -- never a wrong row. There is no
        # fallback into Python by design.
        ("SELECT * FROM t JOIN t AS u ON t.id = u.id", "0A000"),
        ("SELECT avg(n) FROM t", "0A000"),
        ("SELECT n, count(*) FROM t", "42803"),
        ("SELECT * FROM t WHERE n LIKE 'x'", "0A000"),
        ("SELECT * FROM t ORDER BY n + 1", "0A000"),
        # The PK is the document's `_id`, which storage treats as immutable.
        ("UPDATE t SET id = 2 WHERE id = 1", "0A000"),
        ("UPDATE t SET nope = 1", "42703"),
        ("DELETE FROM missing", "42P01"),
    ],
)
def test_refusals_carry_the_right_sqlstate(home: Path, sql: str, sqlstate: str) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, name text, n int)")
        with pytest.raises(psycopg.Error) as exc:
            cur.execute(sql)
    assert exc.value.diag.sqlstate == sqlstate


def test_acknowledged_writes_survive_sigterm(home: Path) -> None:
    """Regression: `secantusd-pg` must close WiredTiger on a signal.

    The first cut had no signal handler, so SIGTERM killed the process with no
    checkpoint. Measured 2026-08-31: after CREATE TABLE + INSERT the client had
    been told both succeeded, and reopening the store found the catalog document
    AND the rows gone. The server acknowledged writes it then lost -- which in a
    database is the whole ballgame, not a tidiness issue.

    `_Server.__exit__` sends SIGTERM, so this asserts the real path.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE durable (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO durable VALUES (1, 10), (2, 20)")
        cur.execute("SELECT id FROM durable")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 2]

    # Nothing above ran a checkpoint explicitly; only the signal handler can
    # have flushed this.
    assert _python_sql(home, "SELECT id, n FROM durable ORDER BY id") == [(1, 10), (2, 20)]


def test_order_limit_offset_and_dml(home: Path) -> None:
    """The P5 slice end to end over the wire.

    Every expectation here was checked against a live PostgreSQL 14; the
    exhaustive comparison lives in `test_rust_pgserver_differential.py`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, n int, s text)")
        cur.execute("INSERT INTO t VALUES (1,3,'c'),(2,NULL,'a'),(3,1,NULL),(4,2,'b')")

        # PostgreSQL puts NULLs LAST on ASC and FIRST on DESC. MongoDB sorts
        # null LOW, so getting this wrong reorders every nullable column.
        cur.execute("SELECT id FROM t ORDER BY n")
        assert [r[0] for r in cur.fetchall()] == [3, 4, 1, 2]
        cur.execute("SELECT id FROM t ORDER BY n DESC")
        assert [r[0] for r in cur.fetchall()] == [2, 1, 4, 3]
        cur.execute("SELECT id FROM t ORDER BY n ASC NULLS FIRST")
        assert [r[0] for r in cur.fetchall()] == [2, 3, 4, 1]

        cur.execute("SELECT id FROM t ORDER BY id LIMIT 2 OFFSET 1")
        assert [r[0] for r in cur.fetchall()] == [2, 3]

        # Three-valued logic: the NULL row is excluded by <> and by NOT IN.
        cur.execute("SELECT id FROM t WHERE n <> 1")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]
        cur.execute("SELECT id FROM t WHERE n NOT IN (1)")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]
        cur.execute("SELECT id FROM t WHERE n NOT IN (1, NULL)")
        assert cur.fetchall() == []
        cur.execute("SELECT id FROM t WHERE NOT (n = 1)")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]

        # UPDATE's row count is rows MATCHED, as PostgreSQL reports.
        cur.execute("UPDATE t SET s = 'z' WHERE n > 1")
        assert cur.rowcount == 2
        cur.execute("SELECT id FROM t WHERE s = 'z'")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 4]

        cur.execute("DELETE FROM t WHERE n IS NULL")
        assert cur.rowcount == 1
        cur.execute("SELECT id FROM t")
        assert sorted(r[0] for r in cur.fetchall()) == [1, 3, 4]


def test_parameterised_queries_go_over_the_extended_protocol(home: Path) -> None:
    """psycopg switches to Parse/Bind/Execute the moment a query has parameters.

    Before the extended handler existed, that path answered `OK` with ZERO ROWS
    for a query that should return rows -- a wrong answer rather than a missing
    feature, and invisible to every literal-SQL test.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, n int, s text)")
        cur.execute("INSERT INTO t VALUES (1,10,'a'),(2,20,'b'),(3,NULL,'c')")

        cur.execute("SELECT id FROM t WHERE n > %s", (5,))
        assert sorted(r[0] for r in cur.fetchall()) == [1, 2]
        cur.execute("SELECT id FROM t WHERE s = %s", ("b",))
        assert cur.fetchall() == [(2,)]
        cur.execute("SELECT id FROM t WHERE n IN (%s, %s)", (10, 20))
        assert sorted(r[0] for r in cur.fetchall()) == [1, 2]
        cur.execute("SELECT count(*) FROM t WHERE n > %s", (5,))
        assert cur.fetchall() == [(2,)]

        # A bound NULL behaves like a literal one: `= NULL` is never true.
        cur.execute("SELECT id FROM t WHERE n = %s", (None,))
        assert cur.fetchall() == []

        cur.execute("UPDATE t SET n = %s WHERE id = %s", (99, 1))
        assert cur.rowcount == 1
        cur.execute("DELETE FROM t WHERE id = %s", (3,))
        assert cur.rowcount == 1
        cur.execute("INSERT INTO t VALUES (%s, %s, %s)", (4, 40, "d"))
        assert cur.rowcount == 1
        cur.execute("SELECT id, n FROM t ORDER BY id")
        assert cur.fetchall() == [(1, 99), (2, 20), (4, 40)]


def test_drop_table_removes_the_catalog_entry(home: Path) -> None:
    """A dropped table must leave nothing behind.

    The collection AND its `__sql_catalog__` document both go; a surviving
    catalog row pointing at a vanished collection is the unrecoverable half,
    which is why the drop does the collection first.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, n int)")
        cur.execute("INSERT INTO t VALUES (1, 10)")
        cur.execute("DROP TABLE t")

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT id FROM t")
        assert exc.value.diag.sqlstate == "42P01"

        # Recreating with a DIFFERENT shape proves the old catalog row is gone
        # rather than merely orphaned.
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, s text)")
        cur.execute("INSERT INTO t VALUES (1, 'fresh')")
        cur.execute("SELECT id, s FROM t")
        assert cur.fetchall() == [(1, "fresh")]


def test_drop_table_if_exists_and_missing(home: Path) -> None:
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY)")
        cur.execute("DROP TABLE t")
        # Bare DROP of a missing table is 42P01; IF EXISTS is a no-op that
        # still reports the DROP TABLE tag (probed PG 14).
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("DROP TABLE t")
        assert exc.value.diag.sqlstate == "42P01"
        cur.execute("DROP TABLE IF EXISTS t")
        cur.execute("DROP TABLE IF EXISTS nope1, nope2")


def test_casts_carry_postgres_types_not_value_types(home: Path) -> None:
    """`Describe` precedes `Bind`, so a column's type cannot be read off the
    value — it comes from the cast."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT %s::int", ("42",))
        assert cur.fetchall() == [(42,)]
        assert cur.description[0].type_code == 23  # int4, not varchar
        cur.execute("SELECT 1::text")
        assert cur.fetchall() == [("1",)]
        assert cur.description[0].type_code == 25  # text, NOT varchar (1043)
        cur.execute("SELECT NULL::int")
        assert cur.fetchall() == [(None,)]
        assert cur.description[0].type_code == 23
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'x'::int")
        assert exc.value.diag.sqlstate == "22P02"


def test_row_description_carries_typmod_and_typlen(home: Path) -> None:
    """A `RowDescription` reports each column's declared type-modifier
    (`atttypmod`) and fixed byte width (`typlen`), which psycopg turns into
    `precision` / `scale` / `display_size` / `internal_size`. Metadata only:
    the modifier never changes how a value is decoded.

    Measured against PostgreSQL 16 -- every fmod/fsize below is the exact wire
    value the real server sends for the same `select null::<type>`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        # numeric(p, s): typmod = ((p << 16) | (s & 0x7FF)) + 4, typlen -1.
        cur.execute("SELECT NULL::numeric(10,2)")
        col = cur.description[0]
        assert cur.pgresult.fmod(0) == 655366
        assert cur.pgresult.fsize(0) == -1
        assert (col.precision, col.scale) == (10, 2)
        assert col.internal_size is None

        # A negative scale (PostgreSQL 15+) rides the signed low 11 bits.
        cur.execute("SELECT NULL::numeric(2,-3)")
        assert cur.pgresult.fmod(0) == 133121
        assert (cur.description[0].precision, cur.description[0].scale) == (2, -3)

        # varchar(n): typmod = n + 4 (varlena header), typlen -1.
        cur.execute("SELECT NULL::varchar(42)")
        col = cur.description[0]
        assert cur.pgresult.fmod(0) == 46
        assert cur.pgresult.fsize(0) == -1
        assert col.display_size == 42

        # time(p): typmod = p, typlen 8 fixed.
        cur.execute("SELECT NULL::time(6)")
        col = cur.description[0]
        assert cur.pgresult.fmod(0) == 6
        assert cur.pgresult.fsize(0) == 8
        assert col.precision == 6
        assert col.internal_size == 8

        # interval(p): typmod packs the full-range mask over the precision.
        cur.execute("SELECT NULL::interval(2)")
        assert cur.pgresult.fmod(0) == (0x7FFF << 16) | 2
        assert cur.pgresult.fsize(0) == 16
        assert cur.description[0].precision == 2

        # bit(n) is its own oid (1560) with the length itself as the modifier.
        cur.execute("SELECT NULL::bit(5)")
        col = cur.description[0]
        assert col.type_code == 1560
        assert cur.pgresult.fmod(0) == 5
        assert col.display_size == 5

        # A fixed-width type with no modifier: typlen set, typmod -1.
        cur.execute("SELECT NULL::int4")
        assert cur.pgresult.fmod(0) == -1
        assert cur.pgresult.fsize(0) == 4
        assert cur.description[0].internal_size == 4


def test_constant_expressions_match_postgres(home: Path) -> None:
    """Arithmetic, concatenation and comparison in a SELECT list.

    Two corners were probed rather than assumed: integer division TRUNCATES
    (`7/2` is 3, not 3.5) and `5/0` is `22012`, not a NULL or an infinity.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want, oid in [
            ("SELECT 1+1", 2, 23),
            ("SELECT 7/2", 3, 23),
            ("SELECT 7%2", 1, 23),
            ("SELECT (1+2)*3", 9, 23),
            ("SELECT -3", -3, 23),
            ("SELECT 'a'||'b'", "ab", 25),
            ("SELECT 'n='||1", "n=1", 25),
            ("SELECT 1+NULL", None, 23),
            ("SELECT 1=1", True, 16),
            ("SELECT 1<2", True, 16),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] == want, sql
            assert cur.description[0].type_code == oid, sql

        # The type comes from the OPERATOR, not the value: Describe plans this
        # against a NULL placeholder and must still say int4.
        cur.execute("SELECT %s + 1", (41,))
        assert cur.fetchone()[0] == 42
        assert cur.description[0].type_code == 23

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 5/0")
        assert exc.value.diag.sqlstate == "22012"


def test_scalar_array_any_all(home: Path) -> None:
    """`col <op> ANY(%s)` / `ALL(%s)` with an array parameter.

    This is how psycopg renders an IN-list, so the WHERE + array-parameter form
    is the one that matters. An untyped array parameter arrives as the array
    literal text and is coerced to the column's element type, and a scalar
    compared to an array with no ANY/ALL is 42883.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE aa (id int PRIMARY KEY, s text)")
        cur.execute("INSERT INTO aa VALUES (1,'x'),(2,'y'),(3,'z')")

        cur.execute("SELECT id FROM aa WHERE id = ANY(%s) ORDER BY id", ([1, 3],))
        assert cur.fetchall() == [(1,), (3,)]
        # An untyped TEXT array parameter is coerced from its literal text.
        cur.execute("SELECT id FROM aa WHERE s = ANY(%s) ORDER BY id", (["x", "z"],))
        assert cur.fetchall() == [(1,), (3,)]
        cur.execute("SELECT id FROM aa WHERE id <> ALL(%s) ORDER BY id", ([2],))
        assert cur.fetchall() == [(1,), (3,)]
        # Empty array: ANY matches nothing, ALL matches everything.
        cur.execute("SELECT id FROM aa WHERE id = ANY(%s)", ([],))
        assert cur.fetchall() == []
        cur.execute("SELECT id FROM aa WHERE id <> ALL(%s) ORDER BY id", ([],))
        assert cur.fetchall() == [(1,), (2,), (3,)]
        # A NULL array matches nothing.
        cur.execute("SELECT id FROM aa WHERE id = ANY(%s)", (None,))
        assert cur.fetchall() == []

        # A scalar compared to an array with no ANY/ALL is 42883.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT id FROM aa WHERE s = ARRAY['x']")
        assert exc.value.diag.sqlstate == "42883"


def test_client_encoding_transcodes_both_directions(home: Path) -> None:
    """`client_encoding` is honoured on the way IN as well as OUT.

    Result text, parameters, the QUERY TEXT itself and RowDescription column
    names all travel in the session encoding, in both protocols -- psycopg
    encodes the query with the encoding it learned from ParameterStatus, so a
    server that decodes it as UTF-8 reads `\u20ac` as three mojibake
    characters. A `client_encoding` in the startup packet is applied before
    the first query and re-reported under its canonical name; an unknown one
    fails the connection with PostgreSQL's FATAL 22023.
    """
    with _Server(home) as server:
        with server.connect() as conn:
            cur = conn.cursor()
            cur.execute("SET client_encoding = 'latin9'")
            assert conn.info.encoding == "iso8859-15"
            # Simple protocol: literal, alias and value all round-trip.
            cur.execute("SELECT 'caf\u00e9 \u20ac' AS \"prix \u20ac\"")
            assert cur.fetchone() == ("caf\u00e9 \u20ac",)
            assert cur.description[0].name == "prix \u20ac"
            # Extended protocol: query text and a text parameter.
            cur.execute("SELECT %s || ' \u20ac'", ("caf\u00e9",))
            assert cur.fetchone() == ("caf\u00e9 \u20ac",)
            # Binary results carry the same bytes.
            cur.execute("SELECT '\u20ac'::text", binary=True)
            assert cur.fetchone() == ("\u20ac",)
            # An untranslatable character is 22P05, as PostgreSQL's is
            # (`chr` builds it server-side: the client could not send it).
            with pytest.raises(psycopg.errors.UntranslatableCharacter):
                cur.execute("SELECT chr(20013)")
            cur.execute("SET client_encoding = 'UTF8'")
            assert conn.info.encoding == "utf-8"
            cur.execute("SELECT chr(20013)")
            assert cur.fetchone() == ("\u4e2d",)

        # Startup packet: libpq's PGCLIENTENCODING / the `client_encoding=`
        # conninfo option. Reported canonically (`utf-8` -> `UTF8`).
        for requested, canonical, py_name in (
            ("utf-8", "UTF8", "utf-8"),
            ("iso8859-15", "LATIN9", "iso8859-15"),
        ):
            with psycopg.connect(
                f"host=127.0.0.1 port={server.port} dbname=postgres user=test "
                f"client_encoding={requested}",
                autocommit=True,
            ) as conn:
                assert conn.info.parameter_status("client_encoding") == canonical
                assert conn.info.encoding == py_name
                assert conn.execute("SELECT '\u20ac'").fetchone() == ("\u20ac",)
        with pytest.raises(psycopg.OperationalError) as exc:
            psycopg.connect(
                f"host=127.0.0.1 port={server.port} dbname=postgres user=test "
                "client_encoding=bogus",
                connect_timeout=10,
            )
        assert 'FATAL:  invalid value for parameter "client_encoding": "bogus"' in str(exc.value)


def test_session_settings(home: Path) -> None:
    """SET / SHOW / RESET and the GUC functions.

    Settings are per CONNECTION, as PostgreSQL's are, and the reported column
    name uses PostgreSQL's canonical casing (`SHOW datestyle` answers a column
    called `DateStyle`) because clients match on it.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SHOW client_encoding")
        assert cur.fetchone()[0] == "UTF8"
        cur.execute("SHOW datestyle")
        assert cur.fetchone()[0] == "ISO, MDY"
        assert cur.description[0].name == "DateStyle"

        # Transaction GUCs psycopg reads to learn the connection's defaults.
        # A single-node server with no 2PC reports these fixed values.
        for name, want in (
            ("max_prepared_transactions", "0"),
            ("transaction_isolation", "read committed"),
            ("default_transaction_isolation", "read committed"),
            ("transaction_deferrable", "off"),
            ("default_transaction_read_only", "off"),
        ):
            cur.execute(f"SHOW {name}")
            assert cur.fetchone()[0] == want, name

        cur.execute("SET my.x = '7'")
        assert cur.statusmessage == "SET"
        cur.execute("SELECT current_setting('my.x')")
        assert cur.fetchone()[0] == "7"

        cur.execute("SELECT set_config('my.y', '9', false)")
        assert cur.fetchone()[0] == "9"
        cur.execute("SELECT current_setting('my.y')")
        assert cur.fetchone()[0] == "9"

        # An unknown name errors; with missing_ok it is NULL (probed PG 14).
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT current_setting('nope.zz')")
        assert exc.value.diag.sqlstate == "42704"
        cur.execute("SELECT current_setting('nope.zz', true)")
        assert cur.fetchone()[0] is None

        cur.execute("RESET my.x")
        assert cur.statusmessage == "RESET"

    # A new connection starts from the defaults, not the previous session's.
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.Error):
            cur.execute("SELECT current_setting('my.y')")


def test_transaction_characteristics_reflected_in_gucs(home: Path) -> None:
    """`BEGIN <modes>` sets the `transaction_*` GUCs for the block.

    Values checked against a live PostgreSQL 14: inside the block the GUCs
    reflect the requested characteristics, and after the block ends they revert
    to the session `default_transaction_*`.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE")
        cur.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("serializable", "on", "on")
        cur.execute("COMMIT")

        # After the block, the transaction GUCs are back to the defaults.
        cur.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("read committed", "off", "off")


def test_set_transaction_inside_a_block(home: Path) -> None:
    """`SET TRANSACTION <modes>` sets the current block's characteristics.

    It answers the `SET` command tag and is reflected in the `transaction_*`
    GUCs until the block ends (checked against PostgreSQL 14).
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("BEGIN")
        cur.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        assert cur.statusmessage == "SET"
        cur.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only')"
        )
        assert cur.fetchone() == ("repeatable read", "on")
        cur.execute("ROLLBACK")
        cur.execute("SELECT current_setting('transaction_isolation')")
        assert cur.fetchone()[0] == "read committed"


def test_set_session_characteristics_sets_the_default(home: Path) -> None:
    """`SET SESSION CHARACTERISTICS AS TRANSACTION <modes>` sets the default.

    Outside a block the current `transaction_*` GUCs move with the default, so
    a client reads back its choice immediately (checked against PostgreSQL 14).
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        assert cur.statusmessage == "SET"
        cur.execute(
            "SELECT current_setting('default_transaction_isolation'), "
            "current_setting('transaction_isolation')"
        )
        assert cur.fetchone() == ("serializable", "serializable")


def test_set_config_with_a_bound_parameter(home: Path) -> None:
    """`set_config($1, $2, false)` over the extended protocol round-trips.

    psycopg's transaction-parameter tests call `set_config` with the name and
    value as bound parameters. During the DESCRIBE the value parameters are
    still unbound, so the name argument arrives as NULL -- the plan must fold
    that to NULL rather than error, and the EXECUTE with real values must set
    the GUC.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT set_config(%s, %s, false)",
            ["default_transaction_isolation", "serializable"],
        )
        assert cur.fetchone()[0] == "serializable"
        cur.execute("SELECT current_setting('default_transaction_isolation')")
        assert cur.fetchone()[0] == "serializable"


def test_psycopg_transaction_parameters_round_trip(home: Path) -> None:
    """psycopg's own `.isolation_level` / `.read_only` / `.deferrable`.

    On a non-autocommit connection psycopg emits `BEGIN <modes>` before the
    first statement of each transaction, so the block reflects the client's
    choice and reverts on rollback.
    """
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        conn.isolation_level = psycopg.IsolationLevel.SERIALIZABLE.value
        conn.read_only = True
        conn.deferrable = True
        cur = conn.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("serializable", "on", "on")
        conn.rollback()

        conn.isolation_level = None
        conn.read_only = None
        conn.deferrable = None
        cur = conn.execute(
            "SELECT current_setting('transaction_isolation'), "
            "current_setting('transaction_read_only'), "
            "current_setting('transaction_deferrable')"
        )
        assert cur.fetchone() == ("read committed", "off", "off")
        conn.rollback()


def test_copy_from_stdin(home: Path) -> None:
    """`COPY ... FROM STDIN` in PostgreSQL's text format.

    The escaping is the substance: `\\N` is NULL and is distinct from an empty
    string, and a literal tab inside a value arrives as `\\t` and must not be
    read as a field separator. A chunk boundary can also land anywhere,
    including mid-row, so the data is buffered and parsed only at CopyDone.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp (id int PRIMARY KEY, s text, n int)")
        with cur.copy("COPY cp FROM STDIN") as cp:
            cp.write("1\ta\t10\n2\t\\N\t20\n3\thas\\ttab\t30\n")
        assert cur.statusmessage == "COPY 3"

        cur.execute("SELECT id, s, n FROM cp ORDER BY id")
        rows = cur.fetchall()
        assert rows[0] == (1, "a", 10)
        assert rows[1] == (2, None, 20), "\\N must be NULL, not the string"
        assert rows[2] == (3, "has\ttab", 30), "an escaped tab is data, not a separator"

        # An explicit column list leaves the others NULL.
        cur.execute("CREATE TABLE cp2 (id int PRIMARY KEY, s text, n int)")
        with cur.copy("COPY cp2 (id, n) FROM STDIN") as cp:
            cp.write("7\t70\n")
        cur.execute("SELECT id, s, n FROM cp2")
        assert cur.fetchall() == [(7, None, 70)]

        # A chunk boundary mid-row must not split a value.
        cur.execute("CREATE TABLE cp3 (id int PRIMARY KEY, s text)")
        with cur.copy("COPY cp3 FROM STDIN") as cp:
            cp.write("1\tab")
            cp.write("c\n2\tdef\n")
        cur.execute("SELECT id, s FROM cp3 ORDER BY id")
        assert cur.fetchall() == [(1, "abc"), (2, "def")]


def test_copy_to_stdout_round_trips(home: Path) -> None:
    """`COPY ... TO STDOUT` in text format.

    Was refused until pgwire 0.38 added the copy-out API (0.31 had no way to
    push CopyData rows from the simple handler). The output is byte-identical
    to PostgreSQL's, so it round-trips straight back through COPY FROM.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE cp (id int, s text, n int)")
        cur.execute("INSERT INTO cp VALUES (1,'a',10),(2,NULL,20),(3,'has\ttab',30)")

        with cur.copy("COPY cp TO STDOUT") as cp:
            out = b"".join(cp).decode()
        # PostgreSQL's own text encoding: `\N` for NULL, escaped tabs.
        assert out == "1\ta\t10\n2\t\\N\t20\n3\thas\\ttab\t30\n"

        cur.execute("DELETE FROM cp")
        with cur.copy("COPY cp FROM STDIN") as cp:
            cp.write(out)
        cur.execute("SELECT id, s, n FROM cp ORDER BY id")
        assert cur.fetchall() == [(1, "a", 10), (2, None, 20), (3, "has\ttab", 30)]


def test_copy_binary_round_trips_scalar_types(home: Path) -> None:
    """`COPY ... TO STDOUT (FORMAT BINARY)` then `FROM STDIN (FORMAT BINARY)`.

    The per-field binary layout is PostgreSQL's own -- fixed-width for the
    numeric and temporal types, length-prefixed raw bytes for `bytea`, the
    element format for arrays. Producing it goes through the same `encode_binary`
    codec the SELECT binary path uses, so a round-trip that survives here proves
    both directions agree on the wire form for every scalar the tests exercise.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "CREATE TABLE cb ("
            "a smallint, b integer, c bigint, d real, e double precision, "
            "f boolean, g text, h numeric, i date, j time, "
            "k timestamp, l bytea, m int4[])"
        )
        cur.execute(
            "INSERT INTO cb VALUES "
            "(1, 2, 9000000000, 1.5, 3.25, true, 'hi', 42.50, "
            "'2026-01-15', '12:34:56.5', '2026-01-15 12:34:56.123456', "
            "'\\x0001ff'::bytea, '{1,2,3}'::int4[])"
        )
        cur.execute("INSERT INTO cb VALUES (" + ", ".join(["NULL"] * 13) + ")")

        with cur.copy("COPY cb TO STDOUT (FORMAT BINARY)") as cp:
            blob = b"".join(cp)

        cur.execute("DELETE FROM cb")
        with cur.copy("COPY cb FROM STDIN (FORMAT BINARY)") as cp:
            cp.write(blob)

        cur.execute("SELECT a,b,c,d,e,f,g,h,i,j,k,l,m FROM cb ORDER BY b NULLS LAST")
        rows = cur.fetchall()
        assert rows[0] == (
            1,
            2,
            9000000000,
            1.5,
            3.25,
            True,
            "hi",
            dc.Decimal("42.50"),
            dt.date(2026, 1, 15),
            dt.time(12, 34, 56, 500000),
            dt.datetime(2026, 1, 15, 12, 34, 56, 123456),
            b"\x00\x01\xff",
            [1, 2, 3],
        )
        assert rows[1] == (None,) * 13


def test_date_and_time_columns(home: Path) -> None:
    """`date` and `time` as real column types.

    Stored as canonical text (the representation the Python server uses, since
    both share one store) but REPORTED with their true oids -- 1082 and 1083.
    That distinction decides whether a client hands back a `date` object or a
    string, and psycopg would never have caught it: it decodes varchar to `str`
    either way.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, d date, tm time)")
        cur.execute("INSERT INTO t VALUES (1, '2026-09-01', '12:34:56')")
        cur.execute("SELECT id, d, tm FROM t")
        rows = cur.fetchall()
        assert rows == [(1, dt.date(2026, 9, 1), dt.time(12, 34, 56))]
        assert [c.type_code for c in cur.description] == [23, 1082, 1083]

        # PostgreSQL accepts several spellings and stores exactly one.
        cur.execute("INSERT INTO t VALUES (2, '2026-9-1', '12:34')")
        cur.execute("SELECT d, tm FROM t WHERE id = 2")
        assert cur.fetchall() == [(dt.date(2026, 9, 1), dt.time(12, 34, 0))]

        # 22007 is "not a date"; 22008 is "a date that cannot exist".
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'not-a-date'::date")
        assert exc.value.diag.sqlstate == "22007"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT '2026-02-30'::date")
        assert exc.value.diag.sqlstate == "22008"


def test_datetime_arithmetic_and_special_values(home: Path) -> None:
    """`date + int`, `timestamp + interval`, `interval + interval`, the `epoch`
    literal, and the `24:00` end-of-day time.

    The result-type OIDs matter as much as the values: psycopg picks its loader
    from the DESCRIBED column type, so `timestamp + interval` must describe as
    1114 (not text/int), `interval + interval` as 1186, and `date + int` as
    1082. An out-of-range arithmetic result is rendered in PostgreSQL's own text
    (a year past 9999, or the `BC` era) so the CLIENT's loader is what rejects
    it -- exactly what psycopg's overflow tests assert. `24:00:00` is a valid
    `time` PostgreSQL renders back verbatim; a Python `time` cannot hold it, so
    the loader raises on the way in.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        # epoch special literal on date / timestamp / timestamptz.
        cur.execute("SELECT 'epoch'::date")
        assert cur.fetchone()[0] == dt.date(1970, 1, 1)
        assert cur.description[0].type_code == 1082
        cur.execute("SELECT 'epoch'::date + 1")
        assert cur.fetchone()[0] == dt.date(1970, 1, 2)
        assert cur.description[0].type_code == 1082

        # date arithmetic types and values.
        cur.execute("SELECT '2000-01-01'::date + 5")
        assert cur.fetchone()[0] == dt.date(2000, 1, 6)
        cur.execute("SELECT '2000-01-01'::date - '1999-01-01'::date")
        assert cur.fetchone()[0] == 365
        assert cur.description[0].type_code == 23

        # timestamp + interval describes as timestamp (1114); interval + interval
        # as interval (1186). The value-type alone would have said text.
        cur.execute("SELECT '2000-01-01 00:00:00'::timestamp + '1s'::interval")
        assert cur.fetchone()[0] == dt.datetime(2000, 1, 1, 0, 0, 1)
        assert cur.description[0].type_code == 1114
        cur.execute("SELECT '1 day'::interval + '1s'::interval")
        assert cur.fetchone()[0] == dt.timedelta(days=1, seconds=1)
        assert cur.description[0].type_code == 1186

        # 24:00:00 is a valid time value PostgreSQL renders verbatim; a Python
        # time cannot hold it, so psycopg's loader raises DataError.
        cur.execute("SELECT '24:00'::time::text")
        assert cur.fetchone()[0] == "24:00:00"
        with pytest.raises(psycopg.DataError):
            cur.execute("SELECT '24:00'::time")
            cur.fetchone()

        # An out-of-range date arithmetic result renders in PG text the client
        # cannot load -- year past 9999 and the BC era.
        cur.execute("SELECT ('9999-12-31'::date + 1)::text")
        assert cur.fetchone()[0] == "10000-01-01"
        cur.execute("SELECT ('0001-01-01'::date + -1)::text")
        assert cur.fetchone()[0] == "0001-12-31 BC"


def test_multidimensional_arrays(home: Path) -> None:
    """Multidimensional arrays over the wire, in both formats.

    The values already STORE and text-render; the gap was RETURNING a nested
    array to the client. `int[]` is binary-encodable, so a 2-D column reaches
    the binary encoder in whichever format the cursor asked for -- text (the
    psycopg default) or binary -- and both must reconstruct `[[1, 2], [3, 4]]`.
    A ragged literal is rejected exactly as PostgreSQL rejects it.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE m (id int PRIMARY KEY, a int[], t text[])")
        cur.execute("INSERT INTO m VALUES (1, '{{1,2},{3,4}}', '{{a,b},{c,d}}')")
        for binary in (False, True):
            c = conn.cursor(binary=binary)
            c.execute("SELECT a, t FROM m WHERE id = 1")
            assert c.fetchone() == ([[1, 2], [3, 4]], [["a", "b"], ["c", "d"]]), binary
            assert [d.type_code for d in c.description] == [1007, 1009]

        cur.execute("SELECT ARRAY[[1, 2], [3, 4]]")
        assert cur.fetchone()[0] == [[1, 2], [3, 4]]
        assert cur.description[0].type_code == 1007

        cur.execute("SELECT ARRAY[[[1, 2]], [[3, 4]]]")
        assert cur.fetchone()[0] == [[[1, 2]], [[3, 4]]]
        cur.execute("SELECT ARRAY[[1, NULL], [3, 4]]::int[]")
        assert cur.fetchone()[0] == [[1, None], [3, 4]]
        cur.execute("INSERT INTO m VALUES (2, %s, NULL)", ([[5, 6], [7, 8]],))
        cur.execute("SELECT a FROM m WHERE id = 2")
        assert cur.fetchone()[0] == [[5, 6], [7, 8]]

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT ARRAY[[1, 2], [3]]")
        assert exc.value.diag.sqlstate == "2202E"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT '{{1,2},{3}}'::int[]")
        assert exc.value.diag.sqlstate == "22P02"


def test_uuid_and_timetz_columns(home: Path) -> None:
    """`uuid` and `timetz` as real column types.

    A uuid canonicalises to lowercase 8-4-4-4-12 from any accepted spelling and
    reports oid 2950, so psycopg hands back a `uuid.UUID`. A timetz keeps its
    LITERAL offset -- it is not session-relative the way timestamptz is -- so
    the canonical text is a safe column; it reports oid 1266.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE u (id int PRIMARY KEY, x uuid, t timetz)")
        cur.execute(
            "INSERT INTO u VALUES (1, 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', '12:34:56+02')"
        )
        cur.execute("SELECT x, t FROM u")
        plus_two = dt.timezone(dt.timedelta(hours=2))
        assert cur.fetchall() == [
            (
                uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
                dt.time(12, 34, 56, tzinfo=plus_two),
            ),
        ]
        assert [c.type_code for c in cur.description] == [2950, 1266]

        # A no-hyphen spelling stores the same canonical value.
        cur.execute("INSERT INTO u VALUES (2, 'a0eebc999c0b4ef8bb6d6bb9bd380a11', '08:00-05')")
        cur.execute("SELECT x::text FROM u WHERE id = 2")
        assert cur.fetchall() == [("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",)]

        # A malformed uuid is 22P02.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'not-a-uuid'::uuid")
        assert exc.value.diag.sqlstate == "22P02"

        # timestamptz IS a column now (stored as a UTC instant, rendered in the
        # session zone); see test_timestamptz_columns_render_in_session_zone.
        cur.execute("CREATE TABLE tstz_ok (id int, t timestamptz)")


def test_timestamp_sub_millisecond_invariant(home: Path) -> None:
    """Timestamps keep microseconds BSON cannot hold, via a hidden companion.

    A BSON date is a MILLISECOND count, so `12:34:56.789012` would truncate to
    `.789000`. The Python server stores the truncated date plus the lost 0-999
    microseconds in a `__us_<field>` companion, and this server writes the same
    thing -- the two share one database, so the representation is a contract.

    THE INVARIANT under test: every write must SET or CLEAR the companion. A
    stale one is worse than truncation, because it reports a time that was
    never stored.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, ts timestamp)")
        cur.execute(
            "INSERT INTO t VALUES (1,'2026-09-01 12:34:56.789012'),"
            "(2,'2026-09-01 12:34:56'),(3,'2026-09-01')"
        )
        cur.execute("SELECT id, ts FROM t ORDER BY id")
        assert cur.fetchall() == [
            (1, dt.datetime(2026, 9, 1, 12, 34, 56, 789012)),
            (2, dt.datetime(2026, 9, 1, 12, 34, 56)),
            # A bare date is midnight.
            (3, dt.datetime(2026, 9, 1, 0, 0)),
        ]
        assert cur.description[1].type_code == 1114  # timestamp, not varchar

        # Overwrite a microsecond row with a whole-millisecond value: the
        # companion must be CLEARED, not left behind.
        cur.execute("UPDATE t SET ts = '2026-09-01 00:00:00' WHERE id = 1")
        cur.execute("SELECT ts FROM t WHERE id = 1")
        assert cur.fetchall() == [(dt.datetime(2026, 9, 1, 0, 0),)]

        # ... and setting microseconds again restores it.
        cur.execute("UPDATE t SET ts = '2026-09-01 01:02:03.456789' WHERE id = 1")
        cur.execute("SELECT ts FROM t WHERE id = 1")
        assert cur.fetchall() == [(dt.datetime(2026, 9, 1, 1, 2, 3, 456789),)]


def test_python_server_reads_rust_timestamps(home: Path) -> None:
    """The companion contract, across servers.

    Getting this wrong is silent corruption rather than an error: the Python
    server would read a time the Rust server never wrote.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE t (id int PRIMARY KEY, ts timestamp)")
        cur.execute(
            "INSERT INTO t VALUES (1,'2026-09-01 01:02:03.456789'),(2,'2026-09-01 12:34:56')"
        )

    assert _python_sql(home, "SELECT id, ts FROM t ORDER BY id") == [
        (1, dt.datetime(2026, 9, 1, 1, 2, 3, 456789)),
        (2, dt.datetime(2026, 9, 1, 12, 34, 56)),
    ]


def test_numeric_keeps_its_scale(home: Path) -> None:
    """`numeric` carries scale as part of the VALUE, not as formatting.

    PostgreSQL answers `'1.50'` for `1.50::numeric::text`, not `'1.5'`, and a
    client reading oid 1700 gets a Decimal rather than a float. Storing these
    as doubles would have given the right magnitude under the wrong type — the
    same failure that made a cast integer arrive as a string.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT 1.5")
        assert cur.fetchone()[0] == Decimal("1.5")
        assert cur.description[0].type_code == 1700  # numeric, not float8

        cur.execute("SELECT 1.50::numeric::text")
        assert cur.fetchone()[0] == "1.50"
        cur.execute("SELECT '-0.30'::numeric::text")
        assert cur.fetchone()[0] == "-0.30"

        cur.execute("CREATE TABLE n (id int PRIMARY KEY, amt numeric)")
        cur.execute("INSERT INTO n VALUES (1, 1.50), (2, '0.1')")
        cur.execute("SELECT id, amt FROM n ORDER BY id")
        assert cur.fetchall() == [(1, Decimal("1.50")), (2, Decimal("0.1"))]
        assert cur.description[1].type_code == 1700

        # Beyond 34 significant digits we refuse rather than round: a quietly
        # rounded number is a wrong answer, an error is a missing feature.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT '1.2345678901234567890123456789012345'::numeric")
        assert exc.value.diag.sqlstate == "22003"


def test_arrays_round_trip_with_their_own_oids(home: Path) -> None:
    """Arrays are their own types, and `int[]` is not `int`.

    libpg_query keeps the array-ness of `int[]` in `array_bounds` rather than
    in the type name, so a server that reads only the name types an array
    column as its element type. That looks harmless until a CAST loses its
    brackets too, at which point `%s::text[] = %s::text[]` quietly degrades to
    comparing two rendered strings — which agrees with PostgreSQL often enough
    to pass for correct.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("SELECT ARRAY[1,2,3]::int[]")
        assert cur.fetchone()[0] == [1, 2, 3]
        assert cur.description[0].type_code == 1007  # int4[], not int4

        cur.execute("SELECT ARRAY['a','b']::text[]")
        assert cur.fetchone()[0] == ["a", "b"]
        assert cur.description[0].type_code == 1009  # text[]

        cur.execute("SELECT '{}'::text[]")
        assert cur.fetchone()[0] == []

        # Inside an array two NULLs are EQUAL and a NULL sorts after every
        # non-NULL — neither rule holds for scalar `=`, where `NULL = NULL` is
        # NULL. All four were probed against a live PostgreSQL 14.
        cur.execute("SELECT ARRAY[NULL]::text[] = ARRAY[NULL]::text[]")
        assert cur.fetchone()[0] is True
        cur.execute("SELECT ARRAY['a',NULL]::text[] > ARRAY['a','z']::text[]")
        assert cur.fetchone()[0] is True
        cur.execute("SELECT ARRAY['a']::text[] < ARRAY['a','b']::text[]")
        assert cur.fetchone()[0] is True

        cur.execute("CREATE TABLE a (id int PRIMARY KEY, xs int[], names text[])")
        cur.execute("INSERT INTO a VALUES (1, '{1,2}', '{x,y}')")
        cur.execute("SELECT xs, names FROM a WHERE id = 1")
        assert cur.fetchone() == ([1, 2], ["x", "y"])

        # A nested array round-trips as a nested list (multidimensional arrays
        # are carried over the wire now — see test_multidimensional_arrays).
        cur.execute("SELECT '{{1,2},{3,4}}'::int[]")
        assert cur.fetchone()[0] == [[1, 2], [3, 4]]


def test_simple_query_runs_a_batch_in_one_implicit_transaction(home: Path) -> None:
    """Several commands in one simple query, as PostgreSQL runs them.

    The transaction is the part that cannot be faked afterwards, and both rules
    below were measured against PostgreSQL 14:

    * a failure anywhere in the batch rolls back what earlier commands wrote —
      the batch is one implicit transaction, not a sequence of autocommits;
    * an explicit ``COMMIT`` inside the batch ends that transaction, so what it
      committed survives a later failure.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("select 1; select 2")
        assert cur.fetchall() == [(1,)]
        assert cur.nextset() is True
        assert cur.fetchall() == [(2,)]

        cur.execute("create table t (id int primary key)")

        # A later failure discards the earlier insert.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("insert into t values (1); select * from nosuchtable")
        assert exc.value.diag.sqlstate == "42P01"
        cur.execute("select count(*) from t")
        assert cur.fetchone()[0] == 0

        # An explicit COMMIT inside the batch is a real commit.
        with pytest.raises(psycopg.Error):
            cur.execute("begin; insert into t values (2); commit; select * from nosuchtable")
        cur.execute("select count(*) from t")
        assert cur.fetchone()[0] == 1

        # Empty commands are accepted and produce no result.
        cur.execute("select 1;;")
        assert cur.fetchone() == (1,)
        cur.execute(";")
        assert cur.statusmessage is None

        # The EXTENDED protocol still refuses several commands: it has one
        # parameter list and one row description, which two commands cannot
        # share. PostgreSQL says exactly this, with this SQLSTATE.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select 1; select %s", (2,))
        assert exc.value.diag.sqlstate == "42601"
        assert "cannot insert multiple commands" in str(exc.value)


def test_deallocate_all_is_accepted(home: Path) -> None:
    """`DEALLOCATE ALL` must succeed, and with PostgreSQL's own tag.

    psycopg issues it to reset its prepared-statement cache, but only when the
    connection happens to have one — so refusing it failed a scattered handful
    of tests depending on execution order, which reads as flakiness rather than
    as a missing feature. The prepared-statement store belongs to the wire
    layer here, so there is nothing to free; the tag is what the client needs.

    `DEALLOCATE <name>` of a name that does not exist is PostgreSQL's 26000
    `InvalidSqlStatementName` (probed PG 16), not a no-op and not 0A000.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("DEALLOCATE ALL")
        assert cur.statusmessage == "DEALLOCATE ALL"

        with pytest.raises(psycopg.errors.InvalidSqlStatementName) as exc:
            cur.execute("DEALLOCATE nosuchstmt")
        assert exc.value.diag.sqlstate == "26000"
        assert 'prepared statement "nosuchstmt" does not exist' in str(exc.value)


def test_pg_typeof_reports_the_display_type(home: Path) -> None:
    """`pg_typeof` prints `integer`, not `int4`, and answers a regtype.

    It reports the STATIC type of its argument, which is why `pg_typeof(NULL)`
    is `unknown`: no value could tell us that.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select pg_typeof(1)")
        assert cur.fetchone()[0] == "integer"
        assert cur.description[0].type_code == 2206  # regtype, not text

        for expr, want in [
            ("1::int8", "bigint"),
            ("1.5", "numeric"),
            ("1.5::float8", "double precision"),
            ("'a'::varchar", "character varying"),
            ("'2026-01-01 12:00'::timestamp", "timestamp without time zone"),
            ("ARRAY[1,2]", "integer[]"),
            ("null", "unknown"),
        ]:
            cur.execute(f"select pg_typeof({expr})::text")
            assert cur.fetchone()[0] == want, expr


# (declared type, value). Each is bound over BOTH wire formats: psycopg picks
# binary for most of these by default, and the two paths decode independently,
# so a type can be right in one and wrong in the other.
_BOUND_VALUES = [
    ("numeric", Decimal("1.50")),
    ("numeric", Decimal("0.1")),
    ("numeric", Decimal("-12345.6789")),
    ("numeric", Decimal("0")),
    ("numeric", Decimal("12345678901234567890.123")),
    ("date", dt.date(2026, 9, 2)),
    ("date", dt.date(1970, 1, 1)),
    ("date", dt.date(1999, 12, 31)),
    ("time", dt.time(12, 34, 56)),
    ("time", dt.time(0, 0, 0)),
    ("time", dt.time(23, 59, 59, 123456)),
    ("timestamp", dt.datetime(2026, 9, 2, 12, 34, 56)),
    ("timestamp", dt.datetime(1969, 7, 20, 20, 17, 40)),
    ("timestamp", dt.datetime(2026, 1, 1, 0, 0, 0, 123456)),
    ("int4[]", [1, 2, 3]),
    ("int4[]", []),
    ("int4[]", [1, None, 3]),
    ("text[]", ["a", "b"]),
    ("text[]", ["a", None]),
    ("int8[]", [10**12, 2]),
    ("float8[]", [1.5, 2.5]),
]


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
@pytest.mark.parametrize("typename,value", _BOUND_VALUES, ids=lambda v: str(v)[:26])
def test_bound_parameter_round_trips_in_both_formats(
    home: Path, typename: str, value: object, binary: bool
) -> None:
    """A bound parameter must survive both wire formats unchanged.

    psycopg sends most of these in BINARY by default, and the two formats are
    decoded by separate code, so a type can be right in one and wrong in the
    other — which is exactly what happened: every one of these was refused in
    binary, and `numeric` in *text* was being parsed as a float, so a client
    binding `Decimal("1.50")` got a float that had already lost the scale that
    distinguishes it from `1.5`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute(f"select %s::{typename}", (value,))
        assert cur.fetchone()[0] == value


def test_timestamp_constants_are_not_null(home: Path) -> None:
    """`select '...'::timestamp` must answer the timestamp, not NULL.

    A stored timestamp is reassembled from its column plus a hidden companion
    field carrying sub-millisecond digits. A timestamp CONSTANT never passes
    through a row, so it reached the encoder as that composite with no arm to
    match it and came out as NULL — while the same value read from a column, or
    cast to text, was correct. A wrong answer that only appears in one of three
    paths to the same value.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select '2026-01-01 12:00'::timestamp")
        assert cur.fetchone()[0] == dt.datetime(2026, 1, 1, 12, 0)
        assert cur.description[0].type_code == 1114

        # Sub-millisecond digits survive the same path.
        cur.execute("select '2026-01-01 12:00:00.123456'::timestamp")
        assert cur.fetchone()[0] == dt.datetime(2026, 1, 1, 12, 0, 0, 123456)

        # The other two routes to the same value still agree.
        cur.execute("select '2026-01-01 12:00'::timestamp::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00"
        cur.execute("create table ts (id int primary key, t timestamp)")
        cur.execute("insert into ts values (1, '2026-01-01 12:00')")
        cur.execute("select t from ts where id = 1")
        assert cur.fetchone()[0] == dt.datetime(2026, 1, 1, 12, 0)


def test_timestamptz_renders_in_the_session_zone(home: Path) -> None:
    """`timestamptz` is an instant; what you see is the session's view of it.

    Two sign conventions meet here and they run opposite ways. In
    ``SET TimeZone TO '+02:00'`` the sign is POSIX — positive is *west* of
    Greenwich, so it renders as ``-02``. In a literal like ``'12:00+02'`` the
    sign is the ordinary one, two hours *east*. Both were probed against a live
    PostgreSQL; getting either backwards is invisible under UTC and wrong by
    hours everywhere else.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("set timezone to 'UTC'")
        cur.execute("select '2026-01-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00+00"
        cur.execute("select '2026-01-01 12:00+02'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 10:00:00+00"

        # POSIX sign: '+02:00' is UTC-02.
        cur.execute("set timezone to '+02:00'")
        cur.execute("select '2026-01-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00-02"

        # A named zone carries a DST rule; the same reading differs by season.
        cur.execute("set timezone to 'Europe/Rome'")
        cur.execute("select '2026-01-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00+01"
        cur.execute("select '2026-07-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-07-01 12:00:00+02"

        # An offset may carry seconds.
        cur.execute("set timezone to 'UTC'")
        cur.execute("select '2000-01-01 00:00+01:02:03'::timestamptz::text")
        assert cur.fetchone()[0] == "1999-12-31 22:57:57+00"

        # Its own oid, so a client builds an aware datetime rather than a naive
        # one from the same characters.
        cur.execute("select '2026-01-01 12:00'::timestamptz")
        assert cur.description[0].type_code == 1184
        cur.execute("select pg_typeof('2026-01-01'::timestamptz)::text")
        assert cur.fetchone()[0] == "timestamp with time zone"
        cur.execute("select pg_typeof('12:00'::timetz)::text")
        assert cur.fetchone()[0] == "time with time zone"
        cur.execute("select '12:00+02'::timetz::text")
        assert cur.fetchone()[0] == "12:00:00+02"


def test_bound_aware_datetimes_keep_their_instant(home: Path) -> None:
    """A bound aware datetime must name the same instant PostgreSQL would.

    psycopg sends these in the binary format by default — 8 bytes of
    microseconds from 2000-01-01 — so the text and binary paths are checked
    separately.
    """
    aware = dt.datetime(2026, 1, 1, 12, 0, tzinfo=dt.timezone.utc)
    with _Server(home) as server, server.connect() as conn:
        for binary in (False, True):
            for zone in ("UTC", "Europe/Rome"):
                conn.cursor().execute(f"set timezone to '{zone}'")
                cur = conn.cursor(binary=binary)
                cur.execute("select %s::timestamptz", (aware,))
                assert cur.fetchone()[0] == aware, (binary, zone)


def test_bound_aware_datetime_equals_literal_no_cast(home: Path) -> None:
    """A bound aware datetime compares equal to the ``::timestamptz`` literal
    for the same instant -- WITHOUT an explicit cast on the parameter.

    This is the shape psycopg's own ``test_dump_datetimetz`` asserts
    (``'<expr>'::timestamptz = %(val)s``). The binary parameter path used to
    ship the instant as session-rendered TEXT, whose zone offset was dropped the
    moment the ``=`` re-coerced it against the literal -- so a binary parameter
    landed two hours (the session offset) off and compared FALSE. Text was fine;
    binary was silently wrong, so both formats are checked. The instants span
    the psycopg corpus edges: year 0001, a sub-second fraction, a seconds-only
    offset, and year 9999.
    """
    utc = dt.timezone.utc
    cases = [
        # (bound UTC instant, literal-with-offset that names the same instant)
        (dt.datetime(1, 1, 1, 12, 0, tzinfo=utc), "0001-01-01 00:00-12:00"),
        (dt.datetime(1999, 12, 31, 22, 0, tzinfo=utc), "2000-01-01 00:00+2"),
        (dt.datetime(2000, 12, 31, 21, 59, 59, 999999, tzinfo=utc), "2000-12-31 23:59:59.999999+2"),
        (dt.datetime(1999, 12, 31, 22, 57, 57, tzinfo=utc), "2000-01-01 00:00+01:02:03"),
        # No offset in the literal -> read in the session zone (+02), so the
        # instant is two hours earlier than the wall clock.
        (dt.datetime(9999, 12, 31, 21, 59, 59, 999999, tzinfo=utc), "9999-12-31 23:59:59.999999"),
    ]
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to '-02:00'")
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            for value, expr in cases:
                cur.execute(f"select '{expr}'::timestamptz = %s", (value,))
                assert cur.fetchone()[0] is True, (binary, expr)


def test_regtype_names_a_type(home: Path) -> None:
    """`'int4'::regtype` is the type it names, printed as PostgreSQL prints it."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for given, want in [("int4", "integer"), ("integer", "integer"), ("text", "text")]:
            cur.execute("select %s::regtype::text", (given,))
            assert cur.fetchone()[0] == want


def test_timetz_columns_are_session_independent(home: Path) -> None:
    """A `timetz` column stores its LITERAL offset, stable under any zone.

    Unlike `timestamptz` (whose instant renders in the session zone -- see
    `test_timestamptz_columns_render_in_session_zone`), a `timetz` offset is
    literal: `12:34:56+02` reads back the same under any `SET timezone`, so its
    canonical text is a safe column.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table u (id int primary key, tt timetz)")
        cur.execute("insert into u values (1, '12:34:56+02')")
        for zone in ("UTC", "Asia/Tokyo"):
            cur.execute(f"set timezone = '{zone}'")
            cur.execute("select tt::text from u")
            assert cur.fetchone()[0] == "12:34:56+02", zone


def test_bytea_type_and_functions(home: Path) -> None:
    """`bytea` as a real type: literals, a column, and the byte functions.

    Stored as a `bson.Binary` (the representation the Python server uses, since
    both share one store), reported with oid 17 so psycopg hands back `bytes`,
    and rendered to text as the `\\x…` hex PostgreSQL emits. psycopg sends and
    reads a bytea in the BINARY wire format by default, so the round-trip
    exercises both the binary parameter decoder and the binary result encoder.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # Hex and escape input both parse; output is always hex.
        cur.execute("select '\\x0102ff'::bytea::text")
        assert cur.fetchone()[0] == "\\x0102ff"
        cur.execute("select 'ab\\001c'::bytea::text")
        assert cur.fetchone()[0] == "\\x61620163"

        # A real column, round-tripped through the binary wire format.
        cur.execute("create table b (id int primary key, v bytea)")
        cur.execute("insert into b values (1, %s)", (b"\xca\xfe\x00\x01",))
        cur.execute("select v from b")
        row = cur.fetchone()
        assert bytes(row[0]) == b"\xca\xfe\x00\x01"
        assert cur.description[0].type_code == 17

        # The byte functions.
        cur.execute("select length('\\x0102ff'::bytea), get_byte('\\x0102ff'::bytea, 2)")
        assert cur.fetchone() == (3, 255)
        cur.execute("select set_byte('\\x0102ff'::bytea, 1, 64)::text")
        assert cur.fetchone()[0] == "\\x0140ff"
        cur.execute("select encode('\\x0102ff'::bytea, 'base64')")
        assert cur.fetchone()[0] == "AQL/"
        cur.execute("select decode('AQL/', 'base64')::text")
        assert cur.fetchone()[0] == "\\x0102ff"
        cur.execute("select ('\\x0102'::bytea || '\\xff'::bytea)::text")
        assert cur.fetchone()[0] == "\\x0102ff"

        # Malformed input and an out-of-range index are distinct SQLSTATEs.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select '\\xZZ'::bytea")
        assert exc.value.diag.sqlstate == "22023"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select get_byte('\\x0102'::bytea, 5)")
        assert exc.value.diag.sqlstate == "2202E"


def test_inet_and_cidr_columns(home: Path) -> None:
    """`inet` (oid 869) and `cidr` (oid 650) as real column types.

    Stored as canonical `addr/masklen` text (the Python server's form, one
    shared store) and sent in the BINARY wire format psycopg uses for these
    oids, so a `/32` host comes back as an `IPv4Address` while a shorter prefix
    is an `IPv4Interface` -- the same distinction PostgreSQL makes. `cidr` is
    strict: host bits below the netmask are rejected.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table n (id int primary key, a inet, b cidr)")
        cur.execute(
            "insert into n values (1, '1.2.3.4', '10.0.0.0/8'),"
            " (2, '2001:db8::1/64', '2001:db8::/32'), (3, '1.2.3.4/24', '192.168.1.0/24')"
        )
        cur.execute("select a, b from n order by id")
        rows = cur.fetchall()
        assert rows[0] == (ipaddress.ip_address("1.2.3.4"), ipaddress.ip_network("10.0.0.0/8"))
        assert rows[1] == (
            ipaddress.ip_interface("2001:db8::1/64"),
            ipaddress.ip_network("2001:db8::/32"),
        )
        assert rows[2] == (
            ipaddress.ip_interface("1.2.3.4/24"),
            ipaddress.ip_network("192.168.1.0/24"),
        )
        assert [c.type_code for c in cur.description] == [869, 650]

        # The `::text` cast is network_show -- it always keeps the mask, even a
        # full-host /32, unlike inet_out (which the column read above drops).
        cur.execute("select '1.2.3.4'::inet::text")
        assert cur.fetchone()[0] == "1.2.3.4/32"

        # A malformed address, and a cidr with host bits set, are both 22P02.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select 'notanip'::inet")
        assert exc.value.diag.sqlstate == "22P02"
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select '10.1.2.3/8'::cidr")
        assert exc.value.diag.sqlstate == "22P02"


def test_interval_keeps_three_independent_parts(home: Path) -> None:
    """An interval is months, days and microseconds — separately.

    They cannot be collapsed into one number because a month is 28–31 days
    depending on where you start: `2026-01-31 + '1 mon'` is `2026-02-28`, which
    no fixed count of microseconds expresses. Comparison, by contrast, *does*
    flatten them (30-day months, 24-hour days), so `'1 mon' = '30 days'` is
    true while `+ '1 mon'` and `+ '30 days'` land on different dates.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("select '1d 3h 4m 5.678s'::interval::text")
        assert cur.fetchone()[0] == "1 day 03:04:05.678"
        cur.execute("select 'P1Y2M3D'::interval::text")
        assert cur.fetchone()[0] == "1 year 2 mons 3 days"
        # A negative value pluralises — this is PostgreSQL's own spelling.
        cur.execute("select '-1 day'::interval::text")
        assert cur.fetchone()[0] == "-1 days"
        # The time part is not a clock and may pass 24 hours.
        cur.execute("select '25:00:00'::interval::text")
        assert cur.fetchone()[0] == "25:00:00"

        # Units that end in `s` are not plurals of something shorter.
        cur.execute("select '500 ms'::interval::text, '5 s'::interval::text")
        assert cur.fetchone() == ("00:00:00.5", "00:00:05")

        cur.execute("select '1 mon'::interval = '30 days'::interval")
        assert cur.fetchone()[0] is True
        cur.execute("select ('2026-01-31'::timestamp + '1 mon'::interval)::text")
        assert cur.fetchone()[0] == "2026-02-28 00:00:00"
        cur.execute("select ('2026-01-31'::timestamp + '30 days'::interval)::text")
        assert cur.fetchone()[0] == "2026-03-02 00:00:00"

        cur.execute("select pg_typeof('1 day'::interval)::text")
        assert cur.fetchone()[0] == "interval"


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_bound_interval_round_trips(home: Path, binary: bool) -> None:
    """A bound `timedelta` survives both wire formats.

    The binary form is three parts too — microseconds, days, months — for the
    same reason the value is.
    """
    with _Server(home) as server, server.connect() as conn:
        for value in (
            dt.timedelta(days=1),
            dt.timedelta(days=1, hours=2, minutes=3, seconds=4),
            dt.timedelta(seconds=-1),
            dt.timedelta(microseconds=500000),
            dt.timedelta(0),
        ):
            cur = conn.cursor(binary=binary)
            cur.execute("select %s::interval", (value,))
            assert cur.fetchone()[0] == value


def test_decimal_arithmetic_works_and_stays_exact(home: Path) -> None:
    """Regression: arithmetic on decimal literals must work at all.

    When decimal literals became `numeric` rather than floats, every arithmetic
    operator on them started refusing outright — `select 1.5 + 1.5` was an
    error — and nothing caught it. The exactness is the point of the type:
    `0.1 + 0.2` is `0.3`, and the result *scale* is part of the answer, so
    `1.50 + 1.5` is `3.00` while `1.5 + 1.5` is `3.0`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, want in [
            ("1.5 + 1.5", "3.0"),
            ("1.50 + 1.5", "3.00"),
            ("0.1 + 0.2", "0.3"),
            ("2.5 * 2", "5.0"),
            ("1.50 * 1.50", "2.2500"),
            ("2.00 - 1.0", "1.00"),
            ("-1::numeric", "-1"),
        ]:
            cur.execute(f"select ({expr})::text")
            assert cur.fetchone()[0] == want, expr

        # More digits than a float holds: these are the same f64 and different
        # numerics, so the comparison must not go through one.
        cur.execute("select '12345678901234567890.1'::numeric < '12345678901234567890.2'::numeric")
        assert cur.fetchone()[0] is True

        # Division is refused rather than guessed at: its result scale depends
        # on the operands' weights in a way this server has not measured.
        with pytest.raises(psycopg.Error):
            cur.execute("select 1.5::numeric / 3")


def test_nan_has_a_place_in_the_order(home: Path) -> None:
    """PostgreSQL orders floats totally; IEEE does not.

    NaN equals itself and sorts above every number, infinity included. Rust's
    `partial_cmp` reports each of those comparisons as "no answer", which this
    server turned into an error where PostgreSQL has a result.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr in [
            "'NaN'::float8 = 'NaN'::float8",
            "'NaN'::float8 > 1e308",
            "'NaN'::float8 > 'Infinity'::float8",
            "'Infinity'::float8 > 1e308",
            "-'Infinity'::float8 < -1e308",
            "'NaN'::numeric = 'NaN'::numeric",
        ]:
            cur.execute(f"select {expr}")
            assert cur.fetchone()[0] is True, expr


def test_an_unknown_literal_takes_the_type_beside_it(home: Path) -> None:
    """PostgreSQL resolves an unknown literal to the other operand's type.

    That type then decides both the parse and the error — which is why
    comparing an interval to `'2020-01-01'` is a *bad interval* rather than
    `false`. The rule applies to comparison exactly as it does to arithmetic;
    implementing it for arithmetic alone left five different failures hiding
    behind one "cannot compare these operands" message.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr in [
            "interval '1 day' = '1 day'",
            "'1 day' = interval '1 day'",
            "'2026-01-01'::timestamp = '2026-01-01'",
            "'2026-01-01'::date = '2026-01-01'",
            "ARRAY[1,2] = '{1,2}'",
            "ARRAY['a','b'] = '{a,b}'",
        ]:
            cur.execute(f"select {expr}")
            assert cur.fetchone()[0] is True, expr

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select interval '1 day' = '2020-01-01'")
        assert exc.value.diag.sqlstate == "22007"


# (sql, argument). Each is bound over BOTH wire formats. The point of these is
# that a parameter's MEANING must not depend on the format it arrived in, and
# the two formats are decoded by separate code.
_TYPED_PARAMS = [
    ("select array['a','b'] = %s", ["a", "b"]),
    ("select '1 day'::interval = %s", dt.timedelta(days=1)),
    ("select '2026-01-01 12:00'::timestamp = %s", dt.datetime(2026, 1, 1, 12, 0)),
    ("select '2026-01-01'::date = %s", dt.date(2026, 1, 1)),
    ("select '12:00'::time = %s", dt.time(12, 0)),
    ("select 1.50::numeric = %s", Decimal("1.5")),
    ("select array[1.5::numeric] = %s", [Decimal("1.5")]),
    ("select array['2026-01-01'::date] = %s", [dt.date(2026, 1, 1)]),
    ("select 'abc' = %s", "abc"),
    ("select 5 = %s", 5),
]


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
@pytest.mark.parametrize("sql,arg", _TYPED_PARAMS, ids=lambda v: str(v)[:34])
def test_typed_parameter_compares_equal_in_both_formats(
    home: Path, sql: str, arg: object, binary: bool
) -> None:
    """A parameter must mean the same thing in either wire format.

    The binary path learned arrays, intervals and timestamps; the text path did
    not, so those values fell through to a plain string and `array[...] = %s`
    compared an array against a string. The error said "cannot compare", which
    pointed at comparison when the cause was one layer earlier, in decoding.

    A client may also leave a parameter's type UNSPECIFIED and let the server
    infer it — psycopg does this for lists and datetimes — in which case the
    value arrives as text whatever the format, and is resolved from the operand
    beside it exactly as a bare literal would be.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        cur.execute(sql, (arg,))
        assert cur.fetchone()[0] is True


def test_json_preserves_and_jsonb_normalises(home: Path) -> None:
    """The one difference that matters between the two types.

    `json` validates and stores the text it was given, so whitespace, key order
    and duplicate keys all survive. `jsonb` stores a parsed structure, so it
    comes back with keys sorted, the last of any duplicate pair kept, and one
    canonical spacing.

    Keys sort by BYTE length first and then bytewise, which is neither
    lexicographic nor by character count: `z` (one byte) precedes `é` (two).
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("""select '{"b":1, "a":2}'::json::text""")
        assert cur.fetchone()[0] == '{"b":1, "a":2}'
        cur.execute("""select '{"b":1, "a":2}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"a": 2, "b": 1}'

        cur.execute("""select '{"a":1, "a":2}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"a": 2}'
        cur.execute("""select '{"aa":1,"ab":2,"b":3}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"b": 3, "aa": 1, "ab": 2}'
        cur.execute("""select '{"é":1,"z":2}'::jsonb::text""")
        assert cur.fetchone()[0] == '{"z": 2, "é": 1}'

        # Their own oids, so a client decodes each as the type it is.
        cur.execute("select '{}'::json")
        assert cur.description[0].type_code == 114
        cur.execute("select '{}'::jsonb")
        assert cur.description[0].type_code == 3802


def test_json_binary_wire_form(home: Path) -> None:
    """json and jsonb have a binary wire form, and it follows `client_encoding`.

    PostgreSQL's `json_send` is the text verbatim and `jsonb_send` prefixes a
    one-byte format version (`1`); both are transcoded to the session encoding
    like any text. Before this the server had no binary codec for either, so a
    binary-format `COPY TO` of a json column failed with 22P03 -- and did so
    AFTER CopyOutResponse, which psycopg reports as "cannot mix COPY with other
    operations" (measured against PostgreSQL 16, byte-identical below).
    """
    with _Server(home) as server, server.connect() as conn:
        for jtype, prefix in (("json", b""), ("jsonb", b"\x01")):
            payload = prefix + b'{"a": "\xc3\xa9"}'
            cur = conn.cursor(binary=True)
            cur.execute(f"""select '{{"a": "\u00e9"}}'::{jtype}""")
            assert cur.pgresult.get_value(0, 0) == payload
            assert cur.fetchone()[0] == {"a": "\u00e9"}

            with conn.cursor().copy(
                f"""copy (select '{{"a": "\u00e9"}}'::{jtype}) to stdout (format binary)"""
            ) as cp:
                rows = [bytes(r) for r in cp]
            # signature (11) + flags (4) + extension length (4), then the row:
            # field count (2) + length (4) + payload.
            assert rows[0][19:] == b"\x00\x01" + len(payload).to_bytes(4, "big") + payload
            assert rows[1] == b"\xff\xff"

        # A binary-format PARAMETER takes the same cast as a text one, so a
        # jsonb sent as psycopg's ASCII-escaped dump is normalised to the
        # character (it used to be stored escaped, and compared unequal).
        from psycopg.types.json import Jsonb

        cur = conn.cursor()
        cur.execute(
            """select %b::text, %b::text = '"\u00e0\u20ac"'::jsonb::text""",
            (Jsonb("\u00e0\u20ac"), Jsonb("\u00e0\u20ac")),
        )
        assert cur.fetchone() == ('"\u00e0\u20ac"', True)

        # (Only the wire bytes: psycopg's json loader decodes as UTF-8 whatever
        # the session encoding, and fails the same way against PostgreSQL.)
        conn.execute("set client_encoding to latin1")
        for jtype, prefix in (("json", b""), ("jsonb", b"\x01")):
            cur = conn.cursor(binary=True)
            cur.execute(f"""select '{{"a": "\u00e9"}}'::{jtype}""")
            assert cur.pgresult.get_value(0, 0) == prefix + b'{"a": "\xe9"}'


def test_jsonb_numbers_are_numerics(home: Path) -> None:
    """A `jsonb` number prints the way a `numeric` does.

    So an exponent is expanded, and a trailing zero written in the literal
    survives — it is the value's scale. Routing numbers through a float would
    give the first and lose the second.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for literal, want in [
            ('{"x": 1.10}', '{"x": 1.10}'),
            ('{"n":-1.5e10}', '{"n": -15000000000}'),
            ('{"n":1e3}', '{"n": 1000}'),
            ('{"n":1.5E-3}', '{"n": 0.0015}'),
        ]:
            cur.execute("select %s::jsonb::text", (literal,))
            assert cur.fetchone()[0] == want, literal


def test_malformed_json_is_refused(home: Path) -> None:
    """Invalid JSON is 22P02, and sniffing must not rescue it.

    `'01'` is the interesting one: a bound parameter whose type the client left
    unspecified used to be sniffed into an integer *before* the cast ran, so
    `'01'::json` became `1` and was accepted — invalid JSON turned valid by a
    guess this server made on the client's behalf. Sniffing now requires the
    number to round-trip to the same text.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for bad in ["{bad}", '{"a":}', "[1,]", "01", '{"a":1} x', "", '{"a" 1}']:
            with pytest.raises(psycopg.Error) as exc:
                cur.execute("select %s::json", (bad,))
            assert exc.value.diag.sqlstate == "22P02", bad


def test_scalar_builtins_are_available_bare(home: Path) -> None:
    """A built-in must work without a cast around it.

    The two routes to a value are not the same code: `select upper('a')::text`
    goes through the expression evaluator, while a bare `select upper('a')`
    goes through the target list. Only the first was wired up at first, and a
    probe whose every case carried a `::text` could not see it — every one of
    these passed while the bare form raised "function upper() is not supported".
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, want in [
            ("upper('aB')", "AB"),
            ("length('héllo')", 5),
            ("octet_length('héllo')", 6),
            ("md5('a')", "0cc175b9c0f1b6a831c399e269772661"),
            ("chr(233)", "é"),
            ("ascii('é')", 233),
            ("left('abcde',-2)", "abc"),
            ("split_part('a,b,c',',',2)", "b"),
            ("concat('a',null,'b')", "ab"),
            ("greatest(1,null)", 1),
            ("coalesce(null,1)", 1),
            ("nullif(1,2)", 1),
        ]:
            cur.execute(f"select {expr}")
            assert cur.fetchone()[0] == want, expr


def test_scalar_builtins_report_their_result_type(home: Path) -> None:
    """The result TYPE is as much of the answer as the value.

    `sign` answers `float8` even for an integer argument, and `div` answers
    `numeric` because that is the type it is defined on. `nullif` answers its
    left operand's type even when the result is NULL — and a NULL cannot report
    a type, so reading it from the value gave `text` where PostgreSQL gives
    `int4`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, oid in [
            ("length('abc')", 23),
            ("exp(1)", 701),
            ("sign(-3)", 701),
            ("div(7,3)", 1700),
            ("md5('a')", 25),
            ("starts_with('abc','ab')", 16),
            ("nullif(1,1)", 23),
            ("nullif(1.5,1.5)", 1700),
        ]:
            cur.execute(f"select {expr}")
            assert cur.description[0].type_code == oid, expr


def test_ranges_canonicalise_by_element_type(home: Path) -> None:
    """A range over a discrete type has exactly one spelling.

    PostgreSQL rewrites every bound of a discrete range to `[)`, so `[1,5]` is
    stored and printed as `[1,6)`. Over a continuous type there is no such
    rewrite — there is no "next" number to move the bound to — so
    `[1.0,2.0]::numrange` stays inclusive.

    The split matters because it is what makes two spellings of one range the
    same range: `'[1,5]'::int4range = '[1,6)'::int4range` is true.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("set timezone to 'UTC'")

        for expr, want in [
            ("int4range(1,5)", "[1,5)"),
            ("int4range(1,5,'[]')", "[1,6)"),
            ("'(1,5)'::int4range", "[2,5)"),
            ("'[2026-01-01,2026-01-05]'::daterange", "[2026-01-01,2026-01-06)"),
            # continuous: left alone
            ("'[1.0,2.0]'::numrange", "[1.0,2.0]"),
            # an infinite bound prints as nothing
            ("int4range(null,5)", "(,5)"),
            ("'(,)'::int4range", "(,)"),
            # a range containing nothing is empty however it was written
            ("int4range(1,1)", "empty"),
            # a bound with a space in it is quoted
            (
                "tsrange('2026-01-01','2026-01-02')",
                '["2026-01-01 00:00:00","2026-01-02 00:00:00")',
            ),
        ]:
            cur.execute(f"select ({expr})::text")
            assert cur.fetchone()[0] == want, expr

        cur.execute("select '[1,5]'::int4range = '[1,6)'::int4range")
        assert cur.fetchone()[0] is True

        cur.execute("select int4range(1,5)")
        assert cur.description[0].type_code == 3904  # int4range, not text


def test_range_errors_use_three_different_classes(home: Path) -> None:
    """Three different mistakes, three different SQLSTATEs.

    A crossed bound is a data error, a malformed literal an invalid-text one,
    and bad bound flags a syntax error. Collapsing them onto one code would
    still refuse the query, and would tell the client the wrong thing about why.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, sqlstate in [
            ("int4range(5,1)", "22000"),
            ("'[5,1)'::int4range", "22000"),
            ("'x'::int4range", "22P02"),
            ("int4range(1,5,'x')", "42601"),
        ]:
            with pytest.raises(psycopg.Error) as exc:
                cur.execute(f"select {expr}")
            assert exc.value.diag.sqlstate == sqlstate, expr


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_range_constructor_takes_bound_parameters(home: Path, binary: bool) -> None:
    """`int4range(%s, %s, %s)` must work — including the bounds argument.

    This is a regression test for a describe-time bug. `Describe` runs before
    `Bind`, so every parameter is NULL when the statement is planned. A NULL
    bounds argument *is* an error in PostgreSQL, and treating it as one at plan
    time failed every parameterised range constructor — with a message that
    quoted this server's own internal placeholder text back at the client.

    The two cases have to be told apart at the AST: a `null` written in the
    query is an error, a not-yet-bound parameter is not.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for args, want in [
            ((10, 20, "[]"), Range(10, 21, "[)")),
            ((None, None, "()"), Range(None, None, "()")),
            ((10, None, "[)"), Range(10, None, "[)")),
        ]:
            cur.execute("select int4range(%s::int4, %s::int4, %s)", args)
            assert cur.fetchone()[0] == want, args

        # A literal null for the flags is still the error PostgreSQL reports.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select int4range(1,5,null)")
        assert exc.value.diag.sqlstate == "22000"


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_range_values_bind_in_both_formats(home: Path, binary: bool) -> None:
    """A range sent as a parameter, in either wire format.

    The binary form is a flags byte and then each present bound in the
    element's own binary format — so it needs the element decoder, not a
    range-specific one.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        for sql, value in [
            ("select %s::int4range", Range(10, 20, "[)")),
            ("select %s::int4range", Range(None, 20, "()")),
            ("select %s::int4range", Range(10, None, "[)")),
            ("select %s::int4range", Range(empty=True)),
            ("select %s::numrange", Range(Decimal("1.5"), Decimal("2.5"), "[]")),
            ("select %s::daterange", Range(dt.date(2026, 1, 1), dt.date(2026, 1, 5), "[)")),
        ]:
            cur.execute(sql, (value,))
            assert cur.fetchone()[0] == value, value


def test_timestamp_range_bounds_with_sub_millisecond_digits(home: Path) -> None:
    """A `tsrange` has to ORDER its own bounds, which needs them comparable.

    A timestamp carrying sub-millisecond digits is stored as a composite, and
    two composites had no comparison arm at all — so building the range failed
    with "comparing timestamp range bounds", a message about ranges for a gap
    that was really in timestamp comparison.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("set timezone to 'UTC'")
        cur.execute("select tsrange('2026-01-01 00:00:00.5','2026-01-02')::text")
        assert cur.fetchone()[0] == '["2026-01-01 00:00:00.5","2026-01-02 00:00:00")'


def test_multiranges_merge_what_touches(home: Path) -> None:
    """A multirange is a normalised set: sorted, empties dropped, and any two
    members that overlap *or merely touch* merged into one.

    Adjacency is the part that is easy to get wrong. `{[1,5),[5,8)}` is
    `{[1,8)}` because nothing lies between them, while `{[1,5),[6,8)}` stays
    two members because 5 does — so the test is "does the next one start at or
    before this one ends", not "do they overlap".
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for expr, want in [
            ("'{[10,20),[1,5)}'::int4multirange", "{[1,5),[10,20)}"),
            ("'{[1,5),[3,8)}'::int4multirange", "{[1,8)}"),
            ("'{[1,5),[5,8)}'::int4multirange", "{[1,8)}"),
            ("'{[1,5),[6,8)}'::int4multirange", "{[1,5),[6,8)}"),
            ("'{[1,2),[2,3),[3,4)}'::int4multirange", "{[1,4)}"),
            ("'{empty}'::int4multirange", "{}"),
            ("'{[1,5),empty,[10,20)}'::int4multirange", "{[1,5),[10,20)}"),
            # members canonicalise first
            ("'{[1,5]}'::int4multirange", "{[1,6)}"),
            ("'{(,5),[10,)}'::int4multirange", "{(,5),[10,)}"),
            # a continuous element type has no adjacency by stepping
            ("'{[1.0,2.0),[2.0,3.0)}'::nummultirange", "{[1.0,3.0)}"),
            ("'{[1.0,2.0),(2.0,3.0)}'::nummultirange", "{[1.0,2.0),(2.0,3.0)}"),
            ("int4multirange()", "{}"),
            ("int4multirange(int4range(1,5),int4range(10,20))", "{[1,5),[10,20)}"),
        ]:
            cur.execute(f"select ({expr})::text")
            assert cur.fetchone()[0] == want, expr

        cur.execute("select '{}'::int4multirange")
        assert cur.description[0].type_code == 4451  # int4multirange, not text


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_multirange_arrays_report_their_element_type(home: Path, binary: bool) -> None:
    """An ARRAY of multiranges is typed as the multirange's array oid, not text.

    Range arrays already reported their element type; multirange arrays fell
    through to varchar. That broke BOTH formats: in text the client read back a
    bare string instead of parsing into Multirange objects, and in binary a
    varchar column stays on the binary path, where the array value could not be
    sent as a binary varchar at all. Their own array oids keep them on the
    text-format path a non-binary-encodable type is already downgraded onto.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute(
            "SELECT ARRAY['{[1,5)}'::int4multirange, '{}'::int4multirange, '{(,)}'::int4multirange]"
        )
        rows = cur.fetchall()
        assert rows == [
            (
                [
                    Multirange([Range(1, 5, "[)")]),
                    Multirange([]),
                    Multirange([Range(None, None, "()")]),
                ],
            )
        ]
        assert cur.description[0].type_code == 6150  # int4multirange[], not varchar


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_multirange_values_bind_in_both_formats(home: Path, binary: bool) -> None:
    """A multirange sent as a parameter, in either wire format.

    Probed alongside the literal form deliberately: the previous batch shipped
    ranges whose literal form was correct in every case and whose parameter
    form was broken in every case, because the probe only covered literals.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        for sql, value in [
            ("select %s::int4multirange", Multirange([Range(1, 5, "[)")])),
            (
                "select %s::int4multirange",
                Multirange([Range(1, 5, "[)"), Range(10, 20, "[)")]),
            ),
            ("select %s::int4multirange", Multirange([])),
            (
                "select %s::nummultirange",
                Multirange([Range(Decimal("1.0"), Decimal("2.0"), "[]")]),
            ),
        ]:
            cur.execute(sql, (value,))
            assert cur.fetchone()[0] == value, value

        cur.execute("select int4multirange(%s::int4range)", (Range(1, 5, "[)"),))
        assert cur.fetchone()[0] == Multirange([Range(1, 5, "[)")])


def test_cursors_follow_postgres_positions(home: Path) -> None:
    """DECLARE / FETCH / MOVE / CLOSE, with PostgreSQL's position model.

    The cursor sits *on* a 1-based row, with 0 before the first and ``len + 1``
    after the last. Those two extra positions are not decoration: fetching past
    the end leaves the cursor at ``len + 1``, so a later ``MOVE BACKWARD 2``
    lands on the **last** row rather than the second-to-last. A simpler
    "index of the next row" model gets that wrong by one.

    Two more rules that only a real server tells you: a BACKWARD fetch returns
    its rows in reverse order, nearest first; and ``RELATIVE``/``ABSOLUTE``
    fetch a *single* row — the n-th from here, or the n-th from the start —
    where ``FORWARD``/``BACKWARD`` fetch a run of them.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, n int)")
        conn.execute("insert into t values (1,10),(2,20),(3,30),(4,40),(5,50)")
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare c1 cursor for select id, n from t order by id")
            assert cur.statusmessage == "DECLARE CURSOR"

            cur.execute("fetch 2 from c1")
            assert cur.fetchall() == [(1, 10), (2, 20)]
            assert cur.statusmessage == "FETCH 2"

            cur.execute("fetch all from c1")
            assert cur.fetchall() == [(3, 30), (4, 40), (5, 50)]

            # Past the end: no rows, and the cursor parks *after* the last row.
            cur.execute("fetch 1 from c1")
            assert cur.fetchall() == []
            assert cur.statusmessage == "FETCH 0"

            # ...which is why backing up two lands on the last row, not the
            # second-to-last.
            cur.execute("move backward 2 in c1")
            assert cur.statusmessage == "MOVE 2"
            cur.execute("fetch 1 from c1")
            assert cur.fetchall() == [(5, 50)]

            # A backward fetch reads in reverse, nearest first.
            cur.execute("fetch backward all from c1")
            assert cur.fetchall() == [(4, 40), (3, 30), (2, 20), (1, 10)]

            # RELATIVE and ABSOLUTE fetch ONE row.
            cur.execute("fetch absolute 2 from c1")
            assert cur.fetchall() == [(2, 20)]
            cur.execute("fetch relative 2 from c1")
            assert cur.fetchall() == [(4, 40)]
            # ABSOLUTE counts from the end when negative.
            cur.execute("fetch absolute -1 from c1")
            assert cur.fetchall() == [(5, 50)]

            cur.execute("close c1")
            assert cur.statusmessage == "CLOSE CURSOR"

        # A cursor needs a transaction; outside one it could never be used.
        with pytest.raises(psycopg.Error) as exc:
            conn.execute("declare c2 cursor for select 1")
        assert exc.value.diag.sqlstate == "25P01"

        with pytest.raises(psycopg.Error) as exc:
            conn.execute("fetch 1 from nosuch")
        assert exc.value.diag.sqlstate == "34000"


def test_repeated_fetch_survives_statement_preparation(home: Path) -> None:
    """Regression: a FETCH must describe the cursor's columns.

    psycopg prepares any statement it runs more than five times, and a prepared
    statement is described once and then executed. The describe path had no arm
    for FETCH, so it reported *zero* columns — and the sixth identical fetch in
    a loop sent rows the client had no description for, which is a protocol
    violation rather than a wrong answer ("D message without prior row
    description").

    Reading a cursor in a loop is the ordinary way to use one, so this affected
    the normal case and not an edge of it.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key)")
        conn.execute("insert into t values (1),(2),(3),(4),(5),(6),(7),(8)")
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare c1 cursor for select id from t order by id")
            seen = []
            # Well past psycopg's prepare threshold of five.
            for _ in range(8):
                cur.execute("fetch 1 from c1")
                assert cur.description is not None, "no row description"
                seen.extend(r[0] for r in cur.fetchall())
            assert seen == [1, 2, 3, 4, 5, 6, 7, 8]


def test_generate_series_is_a_row_source(home: Path) -> None:
    """`generate_series` in FROM, and everything that composes with it.

    It is a *source* rather than a statement of its own, which is the point:
    ORDER BY, LIMIT, OFFSET and the aggregates all work on the generated rows
    without knowing where they came from.

    Two rules worth pinning: counting up towards a smaller stop produces
    nothing rather than reversing (you need a negative step for that), and a
    zero step is refused with `22023` — a value that cannot work, which
    PostgreSQL separates from its generic data-error class.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()

        cur.execute("select * from generate_series(1,5)")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3, 4, 5]
        assert cur.description[0].name == "generate_series"
        assert cur.description[0].type_code == 23

        cur.execute("select * from generate_series(1,10,3)")
        assert [r[0] for r in cur.fetchall()] == [1, 4, 7, 10]
        cur.execute("select * from generate_series(5,1,-2)")
        assert [r[0] for r in cur.fetchall()] == [5, 3, 1]
        # Not reversed — empty.
        cur.execute("select * from generate_series(5,1)")
        assert cur.fetchall() == []

        # The alias renames the column; a column alias beats the table one.
        cur.execute("select * from generate_series(1,3) as g")
        assert cur.description[0].name == "g"
        cur.execute("select * from generate_series(1,3) as g(x)")
        assert cur.description[0].name == "x"

        cur.execute("select * from generate_series(1,3) order by 1 desc")
        assert [r[0] for r in cur.fetchall()] == [3, 2, 1]
        cur.execute("select * from generate_series(1,10) limit 2 offset 3")
        assert [r[0] for r in cur.fetchall()] == [4, 5]

        cur.execute("select count(*) from generate_series(1,100)")
        assert cur.fetchone()[0] == 100
        cur.execute("select sum(g) from generate_series(1,10) g")
        assert cur.fetchone()[0] == 55
        cur.execute("select min(g), max(g) from generate_series(3,7) g")
        assert cur.fetchone() == (3, 7)

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select * from generate_series(1,5,0)")
        assert exc.value.diag.sqlstate == "22023"


def test_a_cursor_over_generate_series(home: Path) -> None:
    """The two features together, which is how the client corpus uses them.

    A cursor needs rows to scroll over, and `generate_series` is the usual way
    to make them without a table — so cursors could be complete and correct
    while every test that used one still failed.
    """
    with _Server(home) as server, server.connect() as conn, conn.transaction():
        cur = conn.cursor()
        cur.execute("declare c cursor for select * from generate_series(1,10)")
        cur.execute("fetch 3 from c")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        cur.execute("fetch backward 2 from c")
        assert [r[0] for r in cur.fetchall()] == [2, 1]
        cur.execute("fetch all from c")
        assert [r[0] for r in cur.fetchall()] == list(range(2, 11))
        cur.execute("close c")


def test_pg_cursors_lists_open_cursors(home: Path) -> None:
    """`pg_cursors` reports THIS connection's open cursors.

    psycopg's server cursor reads it two ways: the suite queries it directly to
    prove a cursor is gone after ``CLOSE``, and the driver itself probes
    ``SELECT 1 FROM pg_catalog.pg_cursors WHERE name = ...`` before closing a
    cursor it did not declare. Both the ``*`` and the ``SELECT 1`` shapes must
    work, and CLOSE must make the row disappear.
    """
    with _Server(home) as server, server.connect() as conn, conn.transaction():
        cur = conn.cursor()
        cur.execute("declare mycur no scroll cursor for select generate_series(1, 5)")

        row = conn.execute(
            "select name, statement, is_holdable, is_binary, is_scrollable "
            "from pg_cursors where name = 'mycur'"
        ).fetchone()
        assert row is not None
        assert row[0] == "mycur"
        assert "generate_series" in row[1]
        assert row[2] is False  # not WITH HOLD
        assert row[3] is False  # not BINARY
        assert row[4] is False  # NO SCROLL

        # The driver's existence probe (the pg_catalog-qualified SELECT 1).
        assert conn.execute(
            "select 1 from pg_catalog.pg_cursors where name = 'mycur'"
        ).fetchone() == (1,)
        # A name that was never declared is simply absent.
        assert conn.execute("select * from pg_cursors where name = 'nope'").fetchone() is None

        cur.execute("close mycur")
        assert conn.execute("select * from pg_cursors where name = 'mycur'").fetchone() is None


def test_constant_select_list_column(home: Path) -> None:
    """`SELECT 1 FROM <source>` — a literal column, one per row.

    The value ignores the row entirely; an unaliased constant is named
    ``?column?`` as PostgreSQL does. This is how a client counts a source's
    rows (``cur.rowcount``) without reading its values, and it works over both a
    real table and a `generate_series`.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key)")
        conn.execute("insert into t values (1),(2),(3)")

        cur = conn.cursor()
        cur.execute("select 1 from t")
        assert cur.fetchall() == [(1,), (1,), (1,)]
        assert cur.rowcount == 3
        assert cur.description[0].name == "?column?"
        assert cur.description[0].type_code == 23  # int4

        # An alias names the column; the value is still the constant.
        cur.execute("select 7 as k from t")
        assert cur.fetchall() == [(7,), (7,), (7,)]
        assert cur.description[0].name == "k"

        # Over a generate_series, including the empty case.
        cur.execute("select 1 from generate_series(1, 42)")
        assert cur.rowcount == 42
        cur.execute("select 1 from generate_series(1, 0)")
        assert cur.fetchall() == []
        assert cur.rowcount == 0


def test_copy_out_formats_keep_null_and_empty_apart(home: Path) -> None:
    """COPY's three formats differ mainly in how they spell NULL.

    * text writes ``\\N`` for NULL; an empty string is an empty field.
    * CSV writes NULL as an *unquoted* empty field, and therefore has to quote
      the empty string as ``""`` to keep the two apart.
    * binary writes a length of −1 for NULL, where an empty string is length 0.

    Every one of those distinctions was broken at some point in writing this,
    and each broke the same way: NULL and empty string became indistinguishable.
    In binary the cause was match-arm order — a catch-all above the NULL arm
    swallowed it and rendered it as text, which is length 0.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cp (id int primary key, s text)")
        conn.execute("insert into cp values (1,'plain'),(2,'has,comma'),(5,NULL),(6,'')")

        with conn.cursor().copy("copy cp to stdout") as cp:
            assert b"".join(cp) == b"1\tplain\n2\thas,comma\n5\t\\N\n6\t\n"

        with conn.cursor().copy("copy cp to stdout with (format csv)") as cp:
            assert b"".join(cp) == b'1,plain\n2,"has,comma"\n5,\n6,""\n'

        with conn.cursor().copy("copy cp to stdout with (format binary)") as cp:
            blob = b"".join(cp)
        assert blob.startswith(b"PGCOPY\n\xff\r\n\x00")
        # NULL is a length of -1, not a zero-length value.
        assert b"\xff\xff\xff\xff" in blob
        assert blob.endswith(b"\xff\xff")


def test_copy_out_from_a_query(home: Path) -> None:
    """`COPY (query) TO STDOUT`, including a query with no FROM at all.

    `copy (select 1) to stdout` is the shape clients use to check that a bad
    query is reported properly, so refusing it failed a whole file's worth of
    tests that had nothing to do with COPY.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cp (id int primary key, s text)")
        conn.execute("insert into cp values (1,'a'),(2,'b'),(3,'c')")

        with conn.cursor().copy("copy (select id from cp order by id) to stdout") as cp:
            assert b"".join(cp) == b"1\n2\n3\n"
        with conn.cursor().copy("copy (select id from cp order by id limit 2) to stdout") as cp:
            assert b"".join(cp) == b"1\n2\n"
        # A generated source works too, since COPY reuses the SELECT path.
        with conn.cursor().copy("copy (select * from generate_series(1,3)) to stdout") as cp:
            assert b"".join(cp) == b"1\n2\n3\n"
        # No FROM at all.
        with conn.cursor().copy("copy (select 1) to stdout") as cp:
            assert b"".join(cp) == b"1\n"


@pytest.mark.parametrize(
    "fmt,data",
    [
        ("", b"1\tplain\n2\thas,comma\n5\t\\N\n6\t\n"),
        ("with (format csv)", b'1,plain\n2,"has,comma"\n5,\n6,""\n'),
    ],
    ids=["text", "csv"],
)
def test_copy_in_formats(home: Path, fmt: str, data: bytes) -> None:
    """COPY FROM in both textual formats, with the NULL rules reversed.

    The CSV parser cannot find rows by splitting on newlines first: a newline
    inside quotes is data.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, s text)")
        with conn.cursor().copy(f"copy t from stdin {fmt}") as cp:
            cp.write(data)
        cur = conn.cursor()
        cur.execute("select id, s from t order by id")
        assert cur.fetchall() == [(1, "plain"), (2, "has,comma"), (5, None), (6, "")]


def test_copy_binary_round_trips(home: Path) -> None:
    """Binary out and back in, through NULL and empty-string values.

    The input side reuses the same per-type decoder as a bound binary
    parameter — the bytes on the wire are identical, so a second
    implementation could only drift from the first.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table a (id int primary key, s text, n int)")
        conn.execute("insert into a values (1,'x',10),(2,NULL,NULL),(3,'',30)")
        conn.execute("create table b (id int primary key, s text, n int)")

        with conn.cursor().copy("copy a to stdout with (format binary)") as cp:
            blob = b"".join(cp)
        with conn.cursor().copy("copy b from stdin with (format binary)") as cp:
            cp.write(blob)

        cur = conn.cursor()
        cur.execute("select id, s, n from b order by id")
        assert cur.fetchall() == [(1, "x", 10), (2, None, None), (3, "", 30)]


def test_copy_from_stdin_serial_column_stores_integers(home: Path) -> None:
    """A `serial` column typed through COPY stores an integer, not a string.

    `serial` is a PostgreSQL pseudo-type: it resolves to `int4`. Before the
    catalog normalised it the column stayed typed `serial`, so a COPY field
    parsed as text and `40010` came back as the string ``"40010"`` — the shape
    psycopg's `test_copy_in_*` cluster caught.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE s (col1 serial primary key, col2 int, data text)")
        with cur.copy("COPY s (col1, col2, data) FROM STDIN") as cp:
            cp.write("40010\t40020\thello\n40040\t\\N\tworld\n")
        cur.execute("SELECT col1, col2, data FROM s ORDER BY col1")
        assert cur.fetchall() == [(40010, 40020, "hello"), (40040, None, "world")]


def test_copy_out_of_a_values_query(home: Path) -> None:
    """`COPY (VALUES ...) TO STDOUT` reads every literal row.

    A bare multi-row ``VALUES`` list is planned as a `ValuesConstant`. Before
    that it fell through the single-row constant path and produced no rows, so
    `copy (values (1),(2)) to stdout` returned an empty body.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        q = (
            "copy (values (40010::int, 40020::int, 'hello'::text), "
            "(40040, NULL, 'world')) to stdout"
        )
        with cur.copy(q) as cp:
            assert b"".join(cp) == b"40010\t40020\thello\n40040\t\\N\tworld\n"
        # The same VALUES executed directly returns its rows, correctly typed.
        cur.execute("values (1, 'a'), (2, 'b')")
        assert cur.fetchall() == [(1, "a"), (2, "b")]


def test_copy_text_round_trips_control_characters(home: Path) -> None:
    """Text COPY preserves the C0 control bytes PostgreSQL backslash-escapes.

    psycopg escapes exactly ``\\b \\t \\n \\v \\f \\r \\\\`` on the way out;
    reading back only ``\\t \\n \\r \\\\`` turned a backspace into a literal
    ``b`` — silent data corruption of any text carrying those bytes.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TABLE c (id int primary key, data text)")
        payload = "".join(chr(b) for b in range(1, 32)) + "\\end"
        with cur.copy("COPY c (id, data) FROM STDIN") as cp:
            cp.write_row([1, payload])
        cur.execute("SELECT data FROM c WHERE id = 1")
        assert cur.fetchone() == (payload,)
        # And it survives a full COPY TO -> COPY FROM round trip.
        with cur.copy("COPY c TO STDOUT") as cp:
            blob = b"".join(cp)
        cur.execute("DELETE FROM c")
        with cur.copy("COPY c FROM STDIN") as cp:
            cp.write(blob)
        cur.execute("SELECT data FROM c WHERE id = 1")
        assert cur.fetchone() == (payload,)


def test_a_table_created_in_a_transaction_is_visible_to_it(home: Path) -> None:
    """`CREATE TABLE` then use it, without committing in between.

    Planning resolves table names against the catalog, and the catalog is an
    ordinary table — so an uncommitted `CREATE TABLE` was invisible to a plain
    read and the next statement reported that the relation did not exist.

    *Execution* already ran inside the transaction, so selecting from a
    pre-existing table worked and hid this completely. What it broke was the
    ordinary shape of a test fixture: create a table, use it, roll back. It
    accounted for 184 psycopg failures on its own.

    Note what this does NOT claim: that the rollback undoes the CREATE. It does
    not — DDL is not transactional on this server, which is a real divergence
    from PostgreSQL and is recorded in `tasks/backlog.md`.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = False
        cur = conn.cursor()

        cur.execute("create table t (id int primary key, s text)")
        cur.execute("select id from t")
        assert cur.fetchall() == []

        cur.execute("insert into t values (1,'x')")
        cur.execute("select id, s from t")
        assert cur.fetchall() == [(1, "x")]

        # COPY into it, still uncommitted. Its rows have to be written inside
        # the transaction like any other write: written outside it, they
        # blocked against the transaction's own locks and hung the connection —
        # a deadlock nobody had seen, because resolving the table failed first
        # whenever a transaction was open.
        with conn.cursor().copy("copy t from stdin") as cp:
            cp.write(b"2\ty\n")
        cur.execute("select count(*) from t")
        assert cur.fetchone()[0] == 2
        conn.rollback()


def test_a_table_dropped_in_a_transaction_is_hidden_from_it(home: Path) -> None:
    """The mirror case: a DROP that has not committed must also be seen.

    The catalog row is deleted but not committed, so a plain read still finds
    it — which would let a statement plan against a table the transaction has
    already dropped.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key)")
        conn.autocommit = False
        cur = conn.cursor()
        cur.execute("drop table t")
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("select id from t")
        assert exc.value.diag.sqlstate == "42P01"
        conn.rollback()


def test_order_by_output_position(home: Path) -> None:
    """`ORDER BY 1` is the first output COLUMN, not the constant 1.

    It is an ordinal into the select list, so it has to be resolved against the
    output columns rather than against the table — `select b, a from t order by
    1` orders by `b`. A position with no such column is PostgreSQL's own
    `42P10`.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table t (id int primary key, a int, b text)")
        conn.execute("insert into t values (1,3,'c'),(2,1,'a'),(3,2,'b')")
        cur = conn.cursor()

        cur.execute("select a, b from t order by 1")
        assert cur.fetchall() == [(1, "a"), (2, "b"), (3, "c")]
        cur.execute("select a, b from t order by 2 desc")
        assert cur.fetchall() == [(3, "c"), (2, "b"), (1, "a")]
        # The ordinal follows the select list, not the table.
        cur.execute("select b, a from t order by 1")
        assert cur.fetchall() == [("a", 1), ("b", 2), ("c", 3)]

        for bad in ("select a from t order by 5", "select a from t order by 0"):
            with pytest.raises(psycopg.Error) as exc:
                cur.execute(bad)
            assert exc.value.diag.sqlstate == "42P10", bad


def test_generate_series_in_the_select_list(home: Path) -> None:
    """`select generate_series(1,3)` is three rows, not one.

    A set-returning function in the target list of a FROM-less query expands
    into rows, so it is planned as a select over a generated source — the same
    shape `FROM generate_series(...)` produces. Clients reach for this form
    constantly to make rows without a table, and a server cursor over it was
    the single biggest use in psycopg's suite.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select generate_series(1,3)")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        assert cur.description[0].name == "generate_series"

        cur.execute("select generate_series(1,3) as g")
        assert cur.description[0].name == "g"
        # Counting up towards a smaller stop is still empty, not reversed.
        cur.execute("select generate_series(3,1)")
        assert cur.fetchall() == []
        cur.execute("select generate_series(1,10) order by 1 desc limit 3")
        assert [r[0] for r in cur.fetchall()] == [10, 9, 8]

        # A server cursor over it, which is how the corpus uses the pair.
        with conn.transaction():
            cur.execute("declare c cursor for select generate_series(1,5)")
            cur.execute("fetch 2 from c")
            assert [r[0] for r in cur.fetchall()] == [1, 2]
            cur.execute("close c")


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_multiranges_bind_in_binary_too(home: Path, binary: bool) -> None:
    """A multirange sent as a bound parameter in either format.

    The binary layout is a count of ranges followed by each one length-prefixed
    in the *range's* own binary form, so it reuses the range decoder rather
    than repeating the flags-and-bounds layout.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("set timezone to 'UTC'")
        cur = conn.cursor(binary=binary)
        for sql, value in [
            ("select %s::int4multirange", Multirange([Range(1, 5, "[)")])),
            (
                "select %s::int4multirange",
                Multirange([Range(1, 5, "[)"), Range(10, 20, "[)")]),
            ),
            ("select %s::int4multirange", Multirange([])),
            (
                "select %s::nummultirange",
                Multirange([Range(Decimal("1.0"), Decimal("2.0"), "[]")]),
            ),
        ]:
            cur.execute(sql, (value,))
            assert cur.fetchone()[0] == value, value


@pytest.mark.parametrize(
    ("sql", "want"),
    [
        ("select 1::int2", 1),
        ("select 1::int4", 1),
        ("select 1::int8", 1),
        ("select 1.5::float4", 1.5),
        ("select 1.5::float8", 1.5),
        ("select true", True),
        ("select 'x'::text", "x"),
        ("select 'x'::varchar", "x"),
        ("select 1.50::numeric", Decimal("1.50")),
        ("select (-1.5)::numeric", Decimal("-1.5")),
        ("select 0.00001::numeric", Decimal("0.00001")),
        ("select 100000::numeric", Decimal("100000")),
        ("select 12345678901234567890.123::numeric", Decimal("12345678901234567890.123")),
        ("select 'NaN'::numeric", Decimal("NaN")),
        ("select array[1,2,3]::int4[]", [1, 2, 3]),
        ("select array['a','b']::text[]", ["a", "b"]),
        ("select array[1.50]::numeric[]", [Decimal("1.50")]),
        ("select null::int4", None),
        ("select null::numeric", None),
    ],
)
def test_binary_result_columns(home: Path, sql: str, want: object) -> None:
    """A cursor that asks for BINARY results is answered in binary.

    The values were always right -- the server described every column as text
    and the client duly decoded text -- so this asserts the FORMAT as well as
    the value, which is the only way the divergence shows up at all.
    """
    with _Server(home) as server, server.connect() as conn:
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            cur.execute(sql)
            got = cur.fetchone()[0]
            assert cur.pgresult.fformat(0) == int(binary), sql
            if isinstance(want, float):
                assert got == pytest.approx(want)
            elif isinstance(want, Decimal) and want.is_nan():
                assert got.is_nan()
            else:
                assert got == want


def test_a_type_without_a_binary_encoding_stays_text(home: Path) -> None:
    """A column this server cannot render in binary is described as text.

    PostgreSQL honours the request for every type. This one honours it for the
    types it can encode exactly and describes the rest as text, which the
    client reads correctly because the format travels per column -- the gap is
    in `tasks/backlog.md`, not hidden behind a wrong answer. (`box` and
    `regtype` are what is left; the datetime family moved to binary, see
    `test_binary_results_cover_every_faker_type`.)
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        cur.execute("select '(1,2),(3,4)'::box")
        assert cur.pgresult.fformat(0) == 0
        assert cur.pgresult.get_value(0, 0) == b"(3,4),(1,2)"


def test_a_server_cursor_describes_its_portal(home: Path) -> None:
    """psycopg's server cursors, which describe the cursor's PORTAL.

    PostgreSQL exposes a declared cursor as a portal of the same name, and
    psycopg describes it straight after the DECLARE -- before any FETCH -- to
    learn the columns. Without an answer for that describe every server cursor
    failed on its first row.
    """
    with _Server(home) as server, server.connect() as conn:
        # A server cursor needs a transaction to declare into, which the
        # autocommit connection the other tests use does not have.
        conn.autocommit = False
        with conn.cursor("c1") as cur:
            cur.execute("select * from generate_series(1, 5) as n")
            assert [d.name for d in cur.description] == ["n"]
            assert cur.fetchmany(2) == [(1,), (2,)]
            assert cur.fetchone() == (3,)
            assert cur.fetchall() == [(4,), (5,)]
        conn.rollback()
        with conn.cursor("c2") as cur:
            cur.execute("select * from generate_series(1, 3) as n")
            assert list(cur) == [(1,), (2,), (3,)]


def test_a_failed_transaction_refuses_everything_until_it_ends(home: Path) -> None:
    """PostgreSQL's failed-transaction block, which this server had no notion of.

    An error inside a transaction aborts the block: every later statement is
    refused with `25P02` until the block ends, and a `COMMIT` there is a
    rollback that says so in its tag. Without this a client that shrugged off a
    mid-transaction error went on writing and committed work PostgreSQL would
    have discarded.
    """
    from psycopg.pq import TransactionStatus

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table ft (id int4)")

        cur.execute("begin")
        assert TransactionStatus(conn.info.transaction_status).name == "INTRANS"
        cur.execute("insert into ft values (1)")
        with pytest.raises(psycopg.errors.UndefinedColumn):
            cur.execute("select nosuchcolumn")
        assert TransactionStatus(conn.info.transaction_status).name == "INERROR"

        # Every shape, including the parameterised one — a separate protocol
        # path that would otherwise sail past the gate.
        for sql, params in [
            ("select 1", ()),
            ("insert into ft values (2)", ()),
            ("select %s", (1,)),
            ("set timezone to 'UTC'", ()),
        ]:
            with pytest.raises(psycopg.errors.InFailedSqlTransaction):
                cur.execute(sql, params)

        cur.execute("commit")
        assert cur.statusmessage == "ROLLBACK"
        assert TransactionStatus(conn.info.transaction_status).name == "IDLE"
        cur.execute("select count(*) from ft")
        assert cur.fetchone()[0] == 0

        # An error OUTSIDE a transaction leaves the session usable.
        with pytest.raises(psycopg.errors.UndefinedColumn):
            cur.execute("select nosuchcolumn")
        assert TransactionStatus(conn.info.transaction_status).name == "IDLE"
        cur.execute("select 1")
        assert cur.fetchone() == (1,)

        # …and a clean transaction still commits, with its own tag.
        cur.execute("begin")
        cur.execute("insert into ft values (3)")
        cur.execute("commit")
        assert cur.statusmessage == "COMMIT"
        cur.execute("select count(*) from ft")
        assert cur.fetchone()[0] == 1


def test_a_series_takes_its_bounds_as_parameters(home: Path) -> None:
    """`generate_series(1, %s)` — the way clients actually write one.

    An untyped bound parameter arrives as text, and PostgreSQL resolves it
    against the function's own signature. Refusing it made every parameterised
    series fail, which is most of them.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select * from generate_series(1, %s)", (4,))
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3, 4]
        cur.execute("select * from generate_series(%s, 10, %s)", (1, 3))
        assert [r[0] for r in cur.fetchall()] == [1, 4, 7, 10]
        cur.execute("select generate_series(1, %s)", (3,))
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        cur.execute("select * from generate_series(1, %s) order by 1 desc limit 2", (5,))
        assert [r[0] for r in cur.fetchall()] == [5, 4]

        # A NULL bound is an EMPTY series, not an error.
        for sql, params in [
            ("select * from generate_series(1, %s)", (None,)),
            ("select * from generate_series(1, null)", ()),
            ("select * from generate_series(1, 10, null)", ()),
        ]:
            cur.execute(sql, params)
            assert cur.fetchall() == [], sql

        # A bound that is not an integer, and one whose type has no overload.
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as text_err:
            cur.execute("select * from generate_series(1, %s)", ("x",))
        assert 'invalid input syntax for type integer: "x"' in str(text_err.value)
        with pytest.raises(psycopg.errors.UndefinedFunction):
            cur.execute("select * from generate_series(1, 3::float8)")


def test_savepoints_undo_only_what_came_after_them(home: Path) -> None:
    """SAVEPOINT / RELEASE / ROLLBACK TO, which is how nested blocks are built.

    WiredTiger has no savepoint of its own, so one here is a set of pre-images:
    before a statement writes a table, every open savepoint that has not yet
    captured that table captures it. The capture and the restore both have to
    go through the transaction's own session — a read outside it cannot see the
    block's uncommitted rows, and a write outside it lands in another snapshot.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table sp (id int4)")

        cur.execute("begin")
        cur.execute("insert into sp values (1)")
        cur.execute("savepoint s1")
        assert cur.statusmessage == "SAVEPOINT"
        cur.execute("insert into sp values (2)")
        cur.execute("rollback to savepoint s1")
        assert cur.statusmessage == "ROLLBACK"
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [1]
        # The savepoint stays open, and captures again from what was restored.
        cur.execute("insert into sp values (3)")
        cur.execute("rollback to savepoint s1")
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [1]
        cur.execute("commit")

        # RELEASE keeps the inner writes, and the OUTER savepoint must still be
        # able to undo them — the released frame's pre-images merge down.
        cur.execute("delete from sp")
        cur.execute("begin")
        cur.execute("insert into sp values (10)")
        cur.execute("savepoint outer_sp")
        cur.execute("insert into sp values (20)")
        cur.execute("savepoint inner_sp")
        cur.execute("insert into sp values (30)")
        cur.execute("release savepoint inner_sp")
        assert cur.statusmessage == "RELEASE"
        cur.execute("rollback to savepoint outer_sp")
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [10]
        cur.execute("commit")

        # A DDL statement inside a savepoint is undone with it.
        cur.execute("begin")
        cur.execute("savepoint ddl")
        cur.execute("create table sp2 (id int4)")
        cur.execute("rollback to savepoint ddl")
        with pytest.raises(psycopg.errors.UndefinedTable):
            cur.execute("select * from sp2")
        cur.execute("rollback")

        # Rolling back to a savepoint un-poisons a failed block.
        cur.execute("begin")
        cur.execute("insert into sp values (40)")
        cur.execute("savepoint ok")
        with pytest.raises(psycopg.errors.UndefinedColumn):
            cur.execute("select nosuchcolumn from sp")
        with pytest.raises(psycopg.errors.InFailedSqlTransaction):
            cur.execute("select 1")
        cur.execute("rollback to savepoint ok")
        cur.execute("select 1")
        assert cur.fetchone() == (1,)
        cur.execute("commit")
        cur.execute("select id from sp order by id")
        assert [r[0] for r in cur.fetchall()] == [10, 40]

        # The names that do not exist, and the block that is not open.
        cur.execute("begin")
        with pytest.raises(psycopg.errors.InvalidSavepointSpecification):
            cur.execute("rollback to savepoint nope")
        cur.execute("rollback")
        for sql in ["savepoint outside", "release outside", "rollback to savepoint outside"]:
            with pytest.raises(psycopg.errors.NoActiveSqlTransaction):
                cur.execute(sql)


def test_a_nested_psycopg_transaction_block(home: Path) -> None:
    """The API that actually reaches the savepoint code.

    psycopg turns a nested `conn.transaction()` into a savepoint, so this is
    the shape a client produces without ever writing the word.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.autocommit = True
        conn.cursor().execute("create table nested (id int4)")
        with conn.transaction():
            conn.cursor().execute("insert into nested values (1)")
            with contextlib.suppress(ZeroDivisionError), conn.transaction():
                conn.cursor().execute("insert into nested values (2)")
                raise ZeroDivisionError("undo the inner block only")
        cur = conn.cursor()
        cur.execute("select id from nested order by id")
        assert [r[0] for r in cur.fetchall()] == [1]


def test_create_table_if_not_exists_is_a_no_op(home: Path) -> None:
    """…on a table that is already there, where a bare CREATE is `42P07`.

    The idiomatic "create it if it is missing" fixture ran twice in one session
    and failed the second time.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table if not exists inex (id int4)")
        cur.execute("insert into inex values (1)")
        cur.execute("create table if not exists inex (id int4)")
        assert cur.statusmessage == "CREATE TABLE"
        # …and it did not empty the table.
        cur.execute("select count(*) from inex")
        assert cur.fetchone()[0] == 1
        with pytest.raises(psycopg.errors.DuplicateTable):
            cur.execute("create table inex (id int4)")


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_a_range_bound_as_a_parameter_is_the_same_range(home: Path, binary: bool) -> None:
    """`int4range(10, 20, '[]') = %s` with the same range bound.

    A range over a discrete element type has one true spelling — PostgreSQL
    rewrites every bound to `[)` — so a parameter that keeps the literal the
    client wrote compares unequal to the same range written any other way while
    printing identically. Two routes had to agree: a range parameter that
    arrives with its type decodes through the cast, and one that arrives
    untyped takes its type from the operand beside it.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for sql, params in [
            ("select int4range(10, 20, '[]') = %s", (Range(10, 20, "[]"),)),
            ("select %s = int4range(10, 21, '[)')", (Range(10, 20, "[]"),)),
            ("select int8range(1, 5, '[]') = %s", (Range(1, 5, "[]"),)),
            # Both sides canonicalise to `[12,21)`; the exclusive lower bound
            # moves up as the inclusive upper one does.
            ("select int4range(11, 20, '(]') = %s", (Range(11, 20, "(]"),)),
            (
                "select numrange(1.0, 2.0, '[]') = %s",
                (Range(Decimal("1.0"), Decimal("2.0"), "[]"),),
            ),
            (
                "select int4multirange(int4range(1, 5, '[]')) = %s",
                (Multirange([Range(1, 5, "[]")]),),
            ),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] is True, sql


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_a_literal_beside_a_range_array_parameter_takes_its_type(home: Path, binary: bool) -> None:
    """`'{"[1,5]"}' = %s` with a LIST of ranges bound: the literal is an
    `int4range[]` and compares as ranges, not as strings.

    psycopg sends `[Int4Range(...)]` as `_int4range` (oid 3905), and the
    planner had no name for that oid -- so `$1` was typed from its decoded
    value (`text[]`), the untyped literal beside it was never coerced, and the
    comparison was refused as `text = text[]`. Every value here was measured
    against PostgreSQL 16: `[1,5]` canonicalises to `[1,6)` on the way in, so
    it equals a bound `[1,6)` and NOT a bound `[1,5)`, a crossed literal is
    22000, and the parameter reports its array type.
    """
    from psycopg.types.multirange import Int4Multirange
    from psycopg.types.range import Int4Range

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute(
            """select '{"[1,5]"}' = %s, pg_typeof(%s)::text""",
            ([Int4Range(1, 5, "[]")], [Int4Range(1, 5, "[]")]),
        )
        assert cur.fetchone() == (True, "int4range[]")
        cur.execute("""select '{"[1,5]"}' = %s""", ([Int4Range(1, 6, "[)")],))
        assert cur.fetchone()[0] is True
        cur.execute("""select '{"[1,5]"}' = %s""", ([Int4Range(1, 5, "[)")],))
        assert cur.fetchone()[0] is False
        # The psycopg gauge's own shape: an empty range and an unbounded one.
        cur.execute(
            """select '{empty,"(,)"}' = %s""",
            ([Int4Range(empty=True), Int4Range(bounds="()")],),
        )
        assert cur.fetchone()[0] is True
        # A multirange array too, through its own array oid.
        mr = [Int4Multirange([Int4Range(1, 6), Int4Range(7, 8)])]
        cur.execute(
            """select '{"{[1,5],[7,8)}"}' = %s, pg_typeof(%s)::text""",
            (mr, mr),
        )
        assert cur.fetchone() == (True, "int4multirange[]")
        # The literal is parsed as the parameter's type, so a crossed pair is
        # the range error, not a string mismatch.
        with pytest.raises(psycopg.errors.DataException) as exc:
            cur.execute("""select '{"[5,1]"}' = %s""", ([Int4Range(1, 5, "[]")],))
        assert "range lower bound must be less than or equal to range upper bound" in str(exc.value)


def test_an_untyped_binary_range_parameter_takes_its_type_from_context(home: Path) -> None:
    """A bare `Range(empty=True)` bound in BINARY beside a typed range.

    psycopg sends an untyped `Range` / `Multirange` with oid 0, and in binary
    that is a flag byte (`\\x01` = empty) or an int32 count -- bytes that are
    only a range once something says which range. PostgreSQL's analysis pass
    gives the parameter the type of the operand it is compared against; the
    server now infers the same from the statement before decoding, so the
    byte reaches the range decoder instead of being read as text (which
    answered False for every one of these). Values measured against
    PostgreSQL 16, including that a parameter with NO context is still 42P18.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        cur.execute("select 'empty'::int4range = %b", (Range(empty=True),))
        assert cur.fetchone()[0] is True
        cur.execute(
            "select 'empty'::numrange = %b, 'empty'::tstzrange = %b",
            (Range(empty=True), Range(empty=True)),
        )
        assert cur.fetchone() == (True, True)
        cur.execute(
            "select '[1,5)'::int4range = %b, %b = '[1,5)'::int4range",
            (Range(1, 5), Range(1, 5)),
        )
        assert cur.fetchone() == (True, True)
        cur.execute("select '[1,5)'::int4range = %b", (Range(1, 6),))
        assert cur.fetchone()[0] is False
        cur.execute(
            "select int4range(NULL::int4, NULL::int4, '()') = %b",
            (Range(None, None, "()"),),
        )
        assert cur.fetchone()[0] is True
        cur.execute("select '{}'::int4multirange = %b", (Multirange(),))
        assert cur.fetchone()[0] is True
        cur.execute(
            "select int4multirange(%s::int4range) = %b",
            (Range(empty=True), Multirange([Range(empty=True)])),
        )
        assert cur.fetchone()[0] is True
        cur.execute("select '{[1,5)}'::int4multirange = %b", (Multirange([Range(1, 5)]),))
        assert cur.fetchone()[0] is True
        # No context, no type: PostgreSQL refuses, and so do we.
        with pytest.raises(psycopg.errors.IndeterminateDatatype) as exc:
            cur.execute(
                "select 'empty'::int4range = %b, pg_typeof(%b)::text",
                (Range(empty=True), Range(empty=True)),
            )
        assert "could not determine data type of parameter $2" in str(exc.value)


def test_custom_range_types_are_created_fetched_and_round_tripped(home: Path) -> None:
    """`CREATE TYPE ... AS RANGE` with the whole psycopg lifecycle around it.

    psycopg's `RangeInfo.fetch` / `register_range` want the type in the
    catalog with its own oid, its subtype oid, and the auto-created
    multirange pointing back at it -- and then the constructor, the casts and
    the dump / load paths in all three formats have to work on the new type.
    Every value here was measured against PostgreSQL 16 by running the same
    script on both servers (`scratchpad/slicecheck.py` during the slice).
    """
    from psycopg.adapt import Dumper
    from psycopg.pq import Format
    from psycopg.types.multirange import MultirangeInfo, register_multirange
    from psycopg.types.range import RangeInfo, register_range

    with _Server(home) as server, server.connect() as conn:
        conn.execute('create type testrange as range (subtype = text, collation = "C")')
        conn.execute("create schema testschema")
        conn.execute("create type testschema.testrange as range (subtype = float8)")
        info = RangeInfo.fetch(conn, "testrange")
        assert info is not None and info.subtype_oid == 25
        register_range(info, conn)
        sinfo = RangeInfo.fetch(conn, "testschema.testrange")
        assert sinfo is not None and sinfo.subtype_oid == 701 and sinfo.oid != info.oid
        register_range(sinfo, conn)
        minfo = MultirangeInfo.fetch(conn, "testmultirange")
        assert minfo is not None and minfo.range_oid == info.oid
        register_multirange(minfo, conn)

        cur = conn.execute(
            "select 'empty'::testrange, '[a,c]'::testrange, pg_typeof('[a,c]'::testrange)::text"
        )
        assert cur.fetchone() == (Range(empty=True), Range("a", "c", "[]"), "testrange")
        cur = conn.execute("select '{[a,c),[e,f]}'::testmultirange")
        assert cur.fetchone()[0] == Multirange([Range("a", "c", "[)"), Range("e", "f", "[]")])
        cur = conn.execute(
            "select testrange('a', 'c'), testrange('a', 'c', '(]'), "
            "testrange('a', NULL), testrange(NULL, NULL, '()')"
        )
        assert cur.fetchone() == (
            Range("a", "c", "[)"),
            Range("a", "c", "(]"),
            Range("a", None, "[)"),
            Range(None, None, "()"),
        )
        cur = conn.execute(
            "select testschema.testrange(1.5, 2.5), '[1.5,2.5)'::testschema.testrange, "
            "pg_typeof(testschema.testrange(1.5, 2.5))::text"
        )
        assert cur.fetchone() == (
            Range(1.5, 2.5, "[)"),
            Range(1.5, 2.5, "[)"),
            "testschema.testrange",
        )
        # The empty-string bound is a value; the quotes are what keep it
        # from being read as an infinite bound.
        cur = conn.execute(
            """select '["",foo)'::testrange, '["",foo)'::testrange::text, """
            """lower('["",foo)'::testrange)"""
        )
        assert cur.fetchone() == (Range("", "foo", "[)"), '["",foo)', "")
        # Dump / load round-trips in every format, including the gauge's
        # quoting-sweep shape: a `"` bound is rendered as a doubled quote.
        for fmt in ("s", "t", "b"):
            cur = conn.execute(
                f"select %{fmt}, %{fmt} = 'empty'::testrange, %{fmt}::text",
                (Range("a", "c"), Range[str](empty=True), Range('"', "#")),
            )
            assert cur.fetchone() == (Range("a", "c", "[)"), True, '["""",#)'), fmt
            cur = conn.execute(f"select %{fmt}", (Multirange([Range("a", "c"), Range("x", None)]),))
            assert cur.fetchone()[0] == Multirange(
                [Range("a", "c", "[)"), Range("x", None, "[)")]
            ), fmt
        # A one-argument constructor call is the cast of a literal, not a
        # lower bound -- so a bare value is the malformed-literal error.
        cur = conn.execute(
            "select testrange('[a,c)'), int4range('[1,3)'), int4multirange('{[1,2)}')"
        )
        assert cur.fetchone() == (
            Range("a", "c", "[)"),
            Range(1, 3, "[)"),
            Multirange([Range(1, 2, "[)")]),
        )
        for sql, literal in [("select testrange('a')", "a"), ("select int4range('1')", "1")]:
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc:
                conn.execute(sql)
            assert exc.value.sqlstate == "22P02"
            assert str(exc.value).splitlines()[0] == f'malformed range literal: "{literal}"'
        # An infinite bound is always exclusive, whatever the subtype.
        cur = conn.execute(
            "select '[,foo)'::testrange::text, '(,5]'::int4range::text, '[,5]'::numrange::text"
        )
        assert cur.fetchone() == ("(,foo)", "(,6)", "(,5]")

        # With the type registered, `Range[str]` is a testrange and the
        # accessors give its bounds (psycopg's `test_dump_quoting` shape).
        cur = conn.execute(
            "select ascii(lower(%(r)s)) = %(low)s and ascii(upper(%(r)s)) = %(up)s",
            {"r": Range('"', "#"), "low": 34, "up": 35},
        )
        assert cur.fetchone() == (True,)

        # A subtype dumper that turns "" into NULL makes the bound infinite,
        # in text and in binary (psycopg's `test_dump_custom_null` shape).
        class StrNoneDumper(Dumper):
            oid = 25

            def dump(self, obj: str) -> bytes | None:
                return obj.encode() if obj else None

        class StrNoneBinaryDumper(StrNoneDumper):
            format = Format.BINARY

        conn.adapters.register_dumper(str, StrNoneDumper)
        conn.adapters.register_dumper(str, StrNoneBinaryDumper)
        for fmt in ("s", "t", "b"):
            cur = conn.execute(f"select %{fmt}::testrange", (Range[str]("", "foo"),))
            assert cur.fetchone()[0] == Range(None, "foo", "()"), fmt


def test_range_accessors_boolean_expressions_and_copy_canonical_form(home: Path) -> None:
    """The pieces the psycopg quoting sweep and COPY tests lean on.

    `lower` / `upper` and the five predicates over every range family
    (NULL for an empty or infinite bound, typed by the subtype), the
    three-valued AND / OR / NOT in a FROM-less select with PostgreSQL's
    42804 for a non-boolean operand and 22P02 for an untyped literal that
    is not a boolean, `ascii` typed as int4, and COPY storing the canonical
    form (`{empty}` is `{}`, `[1,5]` is `[1,6)`). Values measured against
    PostgreSQL 16.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.execute(
            "select lower('[1,5)'::int4range), upper('[1,5)'::int4range), "
            "pg_typeof(lower('[1,5)'::int4range))::text"
        )
        assert cur.fetchone() == (1, 5, "integer")
        cur = conn.execute(
            "select lower('empty'::int4range), upper('empty'::int4range), "
            "lower('(,5)'::int4range), upper('[1,)'::int4range)"
        )
        assert cur.fetchone() == (None, None, None, None)
        cur = conn.execute(
            "select lower_inc('[1,5)'::int4range), upper_inc('[1,5)'::int4range), "
            "lower_inf('(,5)'::int4range), upper_inf('(,5)'::int4range), "
            "isempty('empty'::int4range), isempty('[1,5)'::int4range)"
        )
        assert cur.fetchone() == (True, False, True, False, True, False)
        cur = conn.execute(
            "select lower_inc('empty'::int4range), lower_inf('empty'::int4range), "
            "upper_inf('empty'::int4range)"
        )
        assert cur.fetchone() == (False, False, False)
        cur = conn.execute(
            "select lower('[1.5,2.5]'::numrange), upper('(,)'::numrange), "
            "lower('[2024-01-01,2024-02-01)'::daterange)::text"
        )
        assert cur.fetchone() == (Decimal("1.5"), None, "2024-01-01")
        cur = conn.execute(
            "select lower('{[1,3),[5,7)}'::int4multirange), "
            "upper('{[1,3),[5,7)}'::int4multirange), "
            "isempty('{}'::int4multirange), lower('{}'::int4multirange)"
        )
        assert cur.fetchone() == (1, 7, True, None)
        # An untyped Range goes over as oid 0, and `lower(unknown)` is the
        # TEXT lower -- the range literal itself, case-folded.
        cur = conn.execute(
            "select lower(%s), upper(%s), lower(%s)",
            (Range(1, 5), Range("a", "c"), Range[str](empty=True)),
        )
        assert cur.fetchone() == ("[1,5)", "[A,C)", "empty")
        cur = conn.execute("select lower(NULL::int4range), lower_inc(NULL::int4range)")
        assert cur.fetchone() == (None, None)

        cur = conn.execute(
            "select 1 = 1 and 2 = 2, true or false, not true, 1=1 and 2=3 or 3=3, "
            "true and null, null or true, not null, false and null, null or false"
        )
        assert cur.fetchone() == (True, True, False, True, None, True, None, False, None)
        # Unregistered, the range is text and `lower` / `upper` fold the
        # literal, so `ascii` sees its `[`.
        cur = conn.execute(
            "select ascii(lower(%(r)s)), ascii(upper(%(r)s))", {"r": Range('"', "#")}
        )
        assert cur.fetchone() == (91, 91)
        cur = conn.execute("select ascii(%s)", ("[",))
        assert cur.fetchone() == (91,)
        assert cur.description[0].type_code == 23
        with pytest.raises(psycopg.errors.DatatypeMismatch) as exc:
            conn.execute("select 1 and 2")
        assert exc.value.sqlstate == "42804"
        assert (
            str(exc.value).splitlines()[0]
            == "argument of AND must be type boolean, not type integer"
        )
        cur = conn.execute("select not 'f', 't' and true, 'yes' or null")
        assert cur.fetchone() == (True, True, True)
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc2:
            conn.execute("select not 'x'")
        assert exc2.value.sqlstate == "22P02"
        assert str(exc2.value).splitlines()[0] == 'invalid input syntax for type boolean: "x"'

        conn.execute("create table cpr (id int, r int4range, mr int4multirange)")
        cur = conn.cursor()
        with cur.copy("copy cpr (id, r, mr) from stdin") as cp:
            cp.write("1\t[1,5]\t{empty}\n2\tempty\t{[1,5],[8,9)}\n3\t\\N\t\\N\n")
        cur = conn.execute("select id, r::text, mr::text, r, mr from cpr order by id")
        assert cur.fetchall() == [
            (1, "[1,6)", "{}", Range(1, 6, "[)"), Multirange()),
            (
                2,
                "empty",
                "{[1,6),[8,9)}",
                Range(empty=True),
                Multirange([Range(1, 6, "[)"), Range(8, 9, "[)")]),
            ),
            (3, None, None, None, None),
        ]


def test_pg_typeof_reports_the_type_the_client_declared(home: Path) -> None:
    """…which is not the one the decoded value suggests.

    psycopg sends a small integer as `int2`, so the answer is `smallint` where
    the value alone says `integer`. A parameter the client left untyped has no
    type to report at all, and PostgreSQL answers `42P18` rather than guessing.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for params, want in [
            ((1,), "smallint"),
            ((40000,), "integer"),
            ((10**12,), "bigint"),
            ((1.5,), "double precision"),
            ((Decimal("1.5"),), "numeric"),
            ((True,), "boolean"),
            ((dt.date(2026, 1, 1),), "date"),
            (([1, 2],), "smallint[]"),
        ]:
            cur.execute("select pg_typeof(%s)", params)
            assert cur.fetchone()[0] == want, params

        for params in [("x",), (None,)]:
            with pytest.raises(psycopg.errors.IndeterminateDatatype) as err:
                cur.execute("select pg_typeof(%s)", params)
            assert "could not determine data type of parameter $1" in str(err.value)

        # A cast gives an untyped parameter a type.
        cur.execute("select pg_typeof(%s::int4)", ("5",))
        assert cur.fetchone()[0] == "integer"


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_an_array_takes_its_type_from_its_elements(home: Path, binary: bool) -> None:
    """…not from the values it happens to hold.

    The describe pass sees no values at all — every parameter is NULL there —
    so an array typed from its values described `array[%s::float4]` as
    `text[]`, and the client decoded floats as text because the row description
    is what it believes.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for sql, params, want, oid in [
            ("select array[1,2]", (), [1, 2], 1007),
            ("select array[1::int2]", (), [1], 1005),
            ("select array[1::int8]", (), [1], 1016),
            ("select array[1::float4]", (), [1.0], 1021),
            ("select array[true]", (), [True], 1000),
            ("select array['a'::text]", (), ["a"], 1009),
            ("select array[%s::float4]", ("42",), [42.0], 1021),
            ("select array[%s::int8]", (5,), [5], 1016),
            ("select array[%s]", (5,), [5], 1005),
            # Mixed numerics widen, in PostgreSQL's own order.
            ("select array[1, 1.5]", (), [Decimal("1"), Decimal("1.5")], 1231),
            ("select array[1::float4, 1.5]", (), [1.0, 1.5], 1021),
            ("select array[1::int8, 1::int2]", (), [1, 1], 1016),
            # A bare NULL contributes no type.
            ("select array[null, 1]", (), [None, 1], 1007),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql
            assert cur.pgresult.ftype(0) == oid, sql


def test_a_quoted_brace_is_a_string_not_a_nested_array(home: Path) -> None:
    """`'{"{"}'::text[]` is one element whose text is a brace.

    Only an UNQUOTED `{` opens a sub-array. Treating a quoted one as a nested
    array answered "malformed array literal" for the element — and `{` is an
    ordinary member of any corpus that walks the ASCII range, so it failed
    every round-trip test of a text array.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, params, want in [
            ("select %s::text[]", (["{"],), ["{"]),
            ("select %s::text[]", (["}"],), ["}"]),
            ("select %s::text[]", (["{1,2}"],), ["{1,2}"]),
            ("""select '{"{"}'::text[]""", (), ["{"]),
            ("select %s::varchar[]", (["{"],), ["{"]),
            (
                "select %s::text[]",
                ([chr(i) for i in range(1, 128)],),
                [chr(i) for i in range(1, 128)],
            ),
            # U+0085 and U+00A0 are whitespace to Rust and NOT to PostgreSQL,
            # so trimming an element with `str::trim` returned the empty string
            # for both — a character in, nothing out, and invisible to any test
            # whose alphabet is ASCII.
            ("select %s::text[]", (["\u0085", "\u00a0"],), ["\u0085", "\u00a0"]),
            (
                "select %s::text[]",
                ([chr(i) for i in range(1, 256)],),
                [chr(i) for i in range(1, 256)],
            ),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql


def test_an_array_of_dates_is_described_as_one(home: Path) -> None:
    """A date array was described as `varchar`, so a client read back strings."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select array['2026-01-01'::date]")
        assert cur.fetchone()[0] == [dt.date(2026, 1, 1)]
        assert cur.pgresult.ftype(0) == 1182
        cur.execute("select array['2026-01-01 12:00'::timestamp]")
        assert cur.fetchone()[0] == [dt.datetime(2026, 1, 1, 12)]
        assert cur.pgresult.ftype(0) == 1115


def test_the_json_navigation_operators(home: Path) -> None:
    """`->`, `->>`, `#>`, `#>>` over json and jsonb.

    A json value is carried as its text, so by the time two operands are values
    there is nothing to tell `{"a": 1}` from any other string — the left
    operand's static type is what makes these json operators at all. Every
    lookup that does not apply is SQL NULL rather than an error, which is
    PostgreSQL's rule and the reason they are usable.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want in [
            ("""select '{"a": 1}'::json ->> 'a'""", "1"),
            ("""select '{"a": 1}'::jsonb ->> 'a'""", "1"),
            ("""select '{"a": "x"}'::json ->> 'a'""", "x"),
            ("""select '{"a": "x"}'::json -> 'a'""", "x"),
            ("select '[1,2]'::json ->> 1", "2"),
            # A negative index counts from the end, which is PostgreSQL's rule.
            ("select '[1,2]'::json ->> -1", "2"),
            # Missing, out of range, or the wrong shape: NULL, not an error.
            ("""select '{"a": 1}'::json ->> 'zz'""", None),
            ("select '[1,2]'::json -> 5", None),
            ("""select '"str"'::json ->> 'a'""", None),
            # A json null reads back as SQL NULL through `->>`.
            ("""select '{"a": null}'::json ->> 'a'""", None),
            ("""select '{"a":{"b":2}}'::json #>> '{a,b}'""", "2"),
            ("""select ('{"a":{"b":2}}'::json -> 'a') ->> 'b'""", "2"),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] == want, sql

        # `->` keeps the json flavour, `->>` is text, the key tests are bool.
        for sql, oid in [
            ("""select '{"a": 1}'::json -> 'a'""", 114),
            ("""select '{"a": 1}'::jsonb -> 'a'""", 3802),
            ("""select '{"a": 1}'::json ->> 'a'""", 25),
            ("""select '{"a": 1}'::jsonb ? 'a'""", 16),
        ]:
            cur.execute(sql)
            cur.fetchone()
            assert cur.pgresult.ftype(0) == oid, sql


def test_the_json_key_and_containment_operators(home: Path) -> None:
    """`?`, `?|`, `?&`, `@>` and `<@`.

    Containment compares by VALUE, so key order and whitespace do not count —
    and neither does a number's scale, which is why `{"a": 1.0}` contains
    `{"a": 1}`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want in [
            ("""select '{"a": 1}'::jsonb ? 'a'""", True),
            ("""select '{"a": 1}'::jsonb ? 'z'""", False),
            ("""select '["a","b"]'::jsonb ? 'a'""", True),
            ("""select '{"a": 1}'::jsonb ?| array['a','z']""", True),
            ("""select '{"a": 1}'::jsonb ?& array['a','z']""", False),
            ("""select '{"a": 1, "b": 2}'::jsonb ?& array['a','b']""", True),
            ("""select '{"a": 1, "b": 2}'::jsonb @> '{"a": 1}'""", True),
            ("""select '{"a": 1}'::jsonb @> '{"a": 2}'""", False),
            ("""select '{"a": 1}'::jsonb <@ '{"a": 1, "b": 2}'""", True),
            ("select '[1,2,3]'::jsonb @> '[1,3]'", True),
            # A top-level array contains a bare scalar it holds.
            ("select '[1,2,3]'::jsonb @> '2'", True),
            # …and a number's scale is not part of its value.
            ("""select '{"a": 1.0}'::jsonb @> '{"a": 1}'""", True),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] is want, sql


def test_type_discovery_through_pg_type(home: Path) -> None:
    """psycopg's own `TypeInfo.fetch`, unmodified, against the virtual catalog.

    The query it sends needs five things at once: the `pg_type` table, the
    `to_regtype()` function, the `regtype` cast chain in the select list
    (`oid::regtype::text`), a table alias (`FROM pg_type t ... WHERE t.oid`),
    and column aliases. Any one missing and type discovery fails wholesale.
    """
    from psycopg.types import TypeInfo

    with _Server(home) as server, server.connect() as conn:
        info = TypeInfo.fetch(conn, "text")
        assert (info.name, info.oid, info.array_oid) == ("text", 25, 1009)
        # Both spellings resolve, exactly as PostgreSQL's own catalog does.
        assert TypeInfo.fetch(conn, "integer").oid == 23
        assert TypeInfo.fetch(conn, "int4").oid == 23
        assert TypeInfo.fetch(conn, "jsonb").array_oid == 3807
        # An unknown name is None, not an error — to_regtype's whole point.
        assert TypeInfo.fetch(conn, "nope") is None
        # A QUOTED identifier resolves too, case-sensitively.
        from psycopg import sql as _sql

        assert TypeInfo.fetch(conn, _sql.Identifier("text")).oid == 25
        assert TypeInfo.fetch(conn, _sql.Identifier("TEXT")) is None

        cur = conn.cursor()
        cur.execute("select to_regtype('text')")
        assert cur.fetchone()[0] == "text"
        assert cur.pgresult.ftype(0) == 2206  # regtype
        cur.execute("select to_regtype('nope')")
        assert cur.fetchone()[0] is None
        # A regtype casts onward by its two natures: name as text, oid as int.
        cur.execute("select to_regtype('integer')::text")
        assert cur.fetchone()[0] == "integer"
        cur.execute("select to_regtype('integer')::int4")
        assert cur.fetchone()[0] == 23
        # `::regtype` of an unknown name is an ERROR, unlike to_regtype.
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("select 'nope'::regtype")

        # The catalog rows themselves, filtered and aliased.
        cur.execute("select typname from pg_type where oid = 25")
        assert cur.fetchone()[0] == "text"
        # By now psycopg has run its TypeInfo query past `prepare_threshold`,
        # so the view lists that one server-side statement — PG 16 answers 1
        # here too; the names are psycopg's own `_pg3_N`.
        cur.execute("select name from pg_prepared_statements")
        assert all(name.startswith("_pg3_") for (name,) in cur.fetchall())


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_the_oid_type(home: Path, binary: bool) -> None:
    """`oid` is an unsigned 32-bit integer with its own type oid.

    A negative literal wraps (`(-1)::oid` is 4294967295), a value past 2^32-1
    is out of range, and a non-numeric string is invalid text — all measured on
    PostgreSQL 14. The binary encoding is the 4-byte unsigned form.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        for sql, params, want in [
            ("select 0::oid", (), 0),
            ("select '25'::oid", (), 25),
            ("select 4294967295::oid", (), 4294967295),
            ("select (-1)::oid", (), 4294967295),
            ("select %s::oid", ("0",), 0),
            ("select %s::oid", (25,), 25),
            ("select 25::oid::int4", (), 25),
            ("select 25::oid::text", (), "25"),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql
        cur.execute("select 25::oid")
        cur.fetchone()
        assert cur.pgresult.ftype(0) == 26

        with pytest.raises(psycopg.errors.NumericValueOutOfRange):
            cur.execute("select 4294967296::oid")
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            cur.execute("select 'x'::oid")


def test_a_scalar_call_over_a_column(home: Path) -> None:
    """`regexp_replace(col, ...)` and friends in a table select list.

    The value is computed per row by the executor; the TYPE is fixed at plan
    time, because the describe pass sees no rows and has to name the column's
    type anyway. Routing any function call to the aggregate planner used to
    surface this as a GROUPING error — the wrong error for a plain gap.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table sc (id int4, s text)")
        cur.execute("insert into sc values (1, 'Hello'), (2, 'prepare _pg3_7 as x'), (3, null)")
        cur.execute("select upper(s) from sc order by id")
        assert [r[0] for r in cur.fetchall()] == ["HELLO", "PREPARE _PG3_7 AS X", None]
        cur.execute("select length(s) from sc order by id")
        assert [r[0] for r in cur.fetchall()] == [5, 19, None]
        cur.execute(
            "select regexp_replace(s, 'prepare _pg3_\\d+ as ', '', 'i') as statement"
            " from sc order by id"
        )
        assert [r[0] for r in cur.fetchall()] == ["Hello", "x", None]


def test_regexp_replace(home: Path) -> None:
    """PostgreSQL's rules: FIRST match unless `g`, `\\1` groups, `i` flag."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, want in [
            ("select regexp_replace('aaa', 'a', 'b')", "baa"),
            ("select regexp_replace('aaa', 'a', 'b', 'g')", "bbb"),
            ("select regexp_replace('Hello World', 'o', '0', 'gi')", "Hell0 W0rld"),
            ("select regexp_replace('abc123', '(\\d+)', '<\\1>')", "abc<123>"),
            ("select regexp_replace('a$b', '\\$', 'S')", "aSb"),
        ]:
            cur.execute(sql)
            assert cur.fetchone()[0] == want, sql
        with pytest.raises(psycopg.errors.InvalidRegularExpression):
            cur.execute("select regexp_replace('x', '[', 'y')")
        # A NULL setting name reads back as NULL, not an error.
        cur.execute("select current_setting(%s)", (None,))
        assert cur.fetchone()[0] is None


def test_pg_typeof_answers_a_real_regtype(home: Path) -> None:
    """`pg_typeof(x)::oid` reads the oid, `::text` the name.

    The old string answer could only render; psycopg's wrapper tests read
    `pg_typeof(%s)::oid` for every numeric wrapper, and a display name is not
    a number.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, params, want in [
            ("select pg_typeof(%s)::oid", (1,), 21),
            ("select pg_typeof(%s)::oid", (1.5,), 701),
            ("select pg_typeof(%s)::oid", (Decimal("1"),), 1700),
            ("select pg_typeof(%s)::oid", ([1, 2],), 1005),
            ("select pg_typeof(%s)::text", (1,), "smallint"),
            ("select pg_typeof(1)", (), "integer"),
        ]:
            cur.execute(sql, params)
            assert cur.fetchone()[0] == want, sql


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_a_range_array_carries_its_own_oid(home: Path, binary: bool) -> None:
    """`int4range[]` is 3905, not varchar — a client builds Range objects."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=binary)
        cur.execute("""select '{"[1,5)"}'::int4range[]""")
        assert cur.fetchone()[0] == [Range(1, 5, "[)")]
        assert cur.pgresult.ftype(0) == 3905
        cur.execute("select array['[1,5)'::int4range]")
        assert cur.fetchone()[0] == [Range(1, 5, "[)")]
        assert cur.pgresult.ftype(0) == 3905


def test_enum_ddl_and_the_catalog(home: Path) -> None:
    """CREATE TYPE ... AS ENUM / DROP TYPE, and the catalog reads behind them.

    The 207 test_enum failures all die in one session fixture running exactly
    this DDL, so nothing past it was measurable until it worked. A duplicate
    name is 42710 (distinct from a table's 42P07), a missing one 42704, and a
    case-sensitive name renders QUOTED through regtype — all measured.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type mood as enum ('sad','ok','happy')")
        assert cur.statusmessage == "CREATE TYPE"
        with pytest.raises(psycopg.errors.DuplicateObject):
            cur.execute("create type mood as enum ('x')")
        # The fixture's exact multi-statement shape.
        cur.execute("drop type if exists mood;\ncreate type mood as enum ('sad','ok');")

        cur.execute("select to_regtype('mood')::text")
        assert cur.fetchone()[0] == "mood"
        cur.execute("select typname, typarray from pg_type where oid = to_regtype('mood')")
        name, typarray = cur.fetchone()
        assert name == "mood"
        # typarray is DERIVED as oid + 100_000 — the shared-store rule.
        cur.execute("select oid from pg_type where typname = 'mood'")
        assert typarray == cur.fetchone()[0] + 100_000

        cur.execute("drop type mood")
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("drop type mood")
        cur.execute("select to_regtype('mood')")
        assert cur.fetchone()[0] is None

        # A case-sensitive name, which the CamelCaseEnum fixture uses.
        cur.execute("create type \"CamelCase\" as enum ('x')")
        cur.execute("""select to_regtype('"CamelCase"')::text""")
        assert cur.fetchone()[0] == '"CamelCase"'
        cur.execute('drop type "CamelCase"')


def test_enum_column_reports_its_own_oid(home: Path) -> None:
    """An enum COLUMN is described with the enum's own oid, not varchar.

    psycopg reads that oid to decide whether to apply a registered enum loader:
    with the enum's oid it hands back the Python enum MEMBER, with varchar
    (1043) a bare string. The label is stored and returned verbatim -- including
    a non-ASCII one -- so the case-fold tests turn on the oid, not the bytes.
    """
    import enum as _enum

    from psycopg.types.enum import EnumInfo, register_enum

    class Mood(_enum.Enum):
        sad = "sad"
        ok = "ok"
        happy = "happy"

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type mood as enum ('sad','ok','happy')")
        cur.execute("create table t (id int, m mood, ms mood[])")
        cur.execute("insert into t values (1, 'ok', array['sad','happy']::mood[])")

        # Register the loader, then read through a FRESH cursor. (psycopg binds a
        # cursor's transformer on use, so a cursor that ran the DDL keeps the
        # default loaders; a new cursor picks up the registered enum loader --
        # true against real PostgreSQL too, not a server difference.) With the
        # loader in place the value comes back as the enum MEMBER, scalar and
        # array, and the row description still reports the enum's own minted oid
        # (not varchar 1043) beside an untouched int column.
        register_enum(EnumInfo.fetch(conn, "mood"), conn, Mood)
        rc = conn.cursor()
        rc.execute("select id, m, ms from t")
        id_val, m, ms = rc.fetchone()
        id_oid, m_oid, _ = (d.type_code for d in rc.description)
        assert id_val == 1
        assert m is Mood.ok
        assert ms == [Mood.sad, Mood.happy]
        assert id_oid == 23
        assert m_oid != 1043
        rc.execute("select oid from pg_type where typname = 'mood'")
        assert m_oid == rc.fetchone()[0]

        # ::text stays a plain string (oid 25), unaffected by the loader.
        rc.execute("select m::text from t")
        assert rc.fetchone()[0] == "ok"
        assert rc.description[0].type_code == 25

        # A non-ASCII label is returned VERBATIM (no case fold), which is what
        # the loader maps on.
        cur.execute("create type e as enum ('Xà','sad')")
        cur.execute("create table t2 (m e)")
        cur.execute("insert into t2 values ('Xà')")
        cur.execute("select m::text from t2")
        assert cur.fetchone()[0] == "Xà"


def test_an_enum_created_by_one_server_is_the_other_servers_too(home: Path) -> None:
    """The enum catalog is a SHARED-STORE contract, not an implementation.

    The Rust server writes the Python server's representation — `__sql_enums__`
    docs with monotonically minted oids — so an enum created on one side must
    resolve on the other, with the SAME oid.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type rustmood as enum ('a','b')")
        cur.execute("select oid from pg_type where typname = 'rustmood'")
        rust_oid = cur.fetchone()[0]

    # Python opens the same store and sees the type under the same oid…
    assert _python_sql(home, "SELECT to_regtype('rustmood')::oid") == [(rust_oid,)]
    # …and a type Python creates next mints the NEXT oid, not a reused one.
    _python_sql(home, "CREATE TYPE pymood AS ENUM ('x')")

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select typname, oid from pg_type where oid >= 65000 order by oid")
        rows = cur.fetchall()
        assert rows == [("rustmood", rust_oid), ("pymood", rust_oid + 1)]


def test_range_info_fetch(home: Path) -> None:
    """psycopg's RangeInfo.fetch — pg_range joined to pg_type in a PLAIN select.

    Unlike EnumInfo's aggregate-over-subquery, this is a top-level JOIN with no
    grouping, so it exercises the join source on `Select` rather than
    `Aggregate`. `pg_range` pairs each builtin range with its element oid, from
    the same catalog the range casts read.
    """
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select rngtypid, rngsubtype from pg_range order by rngtypid")
        assert cur.fetchall() == [
            (3904, 23),
            (3906, 1700),
            (3908, 1114),
            (3910, 1184),
            (3912, 1082),
            (3926, 20),
        ]
        for name, oid, sub in [
            ("int4range", 3904, 23),
            ("numrange", 3906, 1700),
            ("tstzrange", 3910, 1184),
            ("daterange", 3912, 1082),
        ]:
            info = RangeInfo.fetch(conn, name)
            assert (info.name, info.oid, info.subtype_oid) == (name, oid, sub), name


def test_composite_info_fetch(home: Path) -> None:
    """psycopg's CompositeInfo.fetch — the 4-layer query behind composite types.

    `pg_type LEFT JOIN (SELECT array_agg(...) FROM (join) GROUP BY ...)` with a
    `coalesce(..., '{}')` per aggregate column. It exercises every composite
    layer at once: a subquery join side materialised from an aggregate, an
    `oid[]` array column (`array_agg(atttypid)`), coalesce projected as a target
    (a real empty array on a miss), and a base type (no fields) surviving the
    LEFT JOIN as two empty arrays. A field whose type is itself a composite
    resolves its own minted oid.
    """
    from psycopg.types.composite import CompositeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type ci_two as (a int, b text)")
        cur.execute("create type ci_varied as (i int, t text, d float8, ts timestamptz, f bool)")
        cur.execute("create type ci_nested as (x int, sub ci_two)")
        cur.execute("select oid from pg_type where typname = 'ci_two'")
        ci_two_oid = cur.fetchone()[0]

        two = CompositeInfo.fetch(conn, "ci_two")
        assert two.name == "ci_two"
        assert list(two.field_names) == ["a", "b"]
        assert list(two.field_types) == [23, 25]  # int4, text -- an oid[], parsed

        varied = CompositeInfo.fetch(conn, "ci_varied")
        assert list(varied.field_names) == ["i", "t", "d", "ts", "f"]
        assert list(varied.field_types) == [23, 25, 701, 1184, 16]

        # A field whose type is itself a composite carries that type's own oid.
        nested = CompositeInfo.fetch(conn, "ci_nested")
        assert list(nested.field_names) == ["x", "sub"]
        assert list(nested.field_types) == [23, ci_two_oid]

        # A base type (no fields) survives the LEFT JOIN: coalesce turns the
        # unmatched NULL side into two EMPTY arrays, not None and not "{}".
        base = CompositeInfo.fetch(conn, "int4")
        assert base.name == "int4"
        assert list(base.field_names) == []
        assert list(base.field_types) == []


def test_composite_binary_result(home: Path) -> None:
    """A composite result column in a BINARY cursor comes back typed.

    psycopg's binary composite loader reads the record wire format -- an int32
    field count, then per field an int32 oid, an int32 length and the bytes --
    and decodes each field by its declared oid. A text cursor renders `(...)`;
    a binary one must send the same values byte-for-byte as PostgreSQL, so the
    float field arrives a float and not the string a text record would carry.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type bc as (foo text, bar int8, baz float8)")
        info = CompositeInfo.fetch(conn, "bc")
        register_composite(info, conn)

        cur = conn.cursor(binary=True)
        res = cur.execute("select row('hi', 10, 20)::bc").fetchone()[0]
        assert res.foo == "hi"
        assert res.bar == 10
        assert res.baz == 20.0
        assert isinstance(res.baz, float)


@pytest.mark.parametrize("binary", [False, True], ids=["text", "binary"])
def test_composite_parameter_round_trips_in_both_formats(home: Path, binary: bool) -> None:
    """A registered composite bound as a PARAMETER decodes to its record value.

    psycopg's `register_composite` sends the composite's OID in the `Parse`
    message and the value in the `Bind` -- but pgwire's `Type::from_oid` maps a
    non-builtin oid to `None`, so the raw oid (and with it any hope of resolving
    the parameter's type) was lost before the parser saw it. The vendored
    pgwire patch preserves the raw oids in `StoredStatement::parameter_oids`, so
    a bound composite now gets a declared type (`pg_typeof` answers the type
    name instead of `could not determine data type of parameter $1`) and its
    value is decoded into the same record BSON a `'(..)'::type` literal
    produces -- from the TEXT `(a,b)` form and the binary RECORD form alike.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type cp as (foo text, bar int4, baz float8)")
        info = CompositeInfo.fetch(conn, "cp")
        register_composite(info, conn)
        obj = info.python_type("hi", 42, 3.5)

        cur = conn.cursor(binary=binary)
        # The Parse wall: the parameter's type is resolved from the raw oid.
        assert cur.execute("select pg_typeof(%s)", [obj]).fetchone()[0] == "cp"
        # The value decodes to the composite and round-trips through a cast.
        got = cur.execute("select %s::cp", [obj]).fetchone()[0]
        assert (got.foo, got.bar, got.baz) == ("hi", 42, 3.5)
        # A NULL field survives, and the text render matches PostgreSQL's.
        obj_null = info.python_type("foo", 1, None)
        assert cur.execute("select %s::text", [obj_null]).fetchone()[0] == "(foo,1,)"
        # The planner NAMES the slot from the compared operand (`row(..)::cp =
        # $1` types `$1` as `cp`), so the parameter arrives with a declared
        # composite type rather than the raw oid alone. That declared type must
        # take the record decoder too: it once fell through to the generic one,
        # which read the text form as a plain string (`comparing document with
        # string`) and refused the binary form outright (`binary parameters of
        # type oid Some(...) are not supported yet`).
        assert cur.execute("select row('hi', 42, 3.5)::cp = %s", [obj]).fetchone()[0] is True
        assert cur.execute("select row('hi', 43, 3.5)::cp = %s", [obj]).fetchone()[0] is False
        # And a composite holding a range field, on a fresh connection (a
        # second `register_composite` on the same connection makes psycopg
        # send the parameter untyped -- PostgreSQL answers `could not
        # determine data type of parameter $1` for that too).
        conn.execute("create type cpr as (num int4, r daterange, nums int4[])")
        conn2 = conn.__class__.connect(conn.info.dsn, autocommit=True)
        from psycopg.types.range import Range

        rinfo = CompositeInfo.fetch(conn2, "cpr")
        register_composite(rinfo, conn2)
        robj = rinfo.python_type(10, Range(empty=True), [])
        cur = conn2.cursor(binary=binary)
        assert cur.execute("select pg_typeof(%s)", [robj]).fetchone()[0] == "cpr"
        assert cur.execute("select %s::text", [robj]).fetchone()[0] == "(10,empty,{})"


def test_composite_array_load_text_and_binary(home: Path) -> None:
    """`array[<composite>]` reports the composite's ARRAY oid, so the client
    parses it as a one-element array of composites rather than a bare string.

    Before the fix the column was described as varchar, so the array text
    `{"(hi,10,30)"}` was handed back as a 17-character string. Both wire formats
    now round-trip the array, in a text cursor and a binary one.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type bc2 as (foo text, bar int8, baz float8)")
        info = CompositeInfo.fetch(conn, "bc2")
        register_composite(info, conn)

        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            res = cur.execute("select array[row('hi', 10, 30)::bc2]").fetchone()[0]
            assert len(res) == 1
            assert res[0].foo == "hi"
            assert res[0].baz == 30.0
            assert isinstance(res[0].baz, float)


def test_composite_recursive_load(home: Path) -> None:
    """A composite whose field is itself a composite round-trips, scalar and in
    an array, in both wire formats -- the nested field is encoded as its own
    record (binary) or quoted `(...)` text (text)."""
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type rc_inner as (foo text, bar int8, baz float8)")
        conn.execute("create type rc_outer as (qux int8, quux rc_inner)")
        register_composite(CompositeInfo.fetch(conn, "rc_inner"), conn)
        register_composite(CompositeInfo.fetch(conn, "rc_outer"), conn)

        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            res = cur.execute("select row(42, row('hi', 10, 20)::rc_inner)::rc_outer").fetchone()[0]
            assert res.qux == 42
            assert res.quux.foo == "hi"
            assert res.quux.baz == 20.0
            assert isinstance(res.quux.baz, float)

            res = cur.execute(
                "select array[row(42, row('hi', 10, 30)::rc_inner)::rc_outer]"
            ).fetchone()[0]
            assert len(res) == 1
            assert res[0].quux.baz == 30.0
            assert isinstance(res[0].quux.baz, float)


def test_schema_qualified_composite_is_distinct(home: Path) -> None:
    """`create type s.t` is a DISTINCT type from a bare `t`.

    psycopg's composite test fixture creates `testschema.testcomp` beside a bare
    `testcomp`; a server that resolves a qualified type name to its last part
    collides them and the CREATE fails 42710, which -- the fixture being
    session-scoped -- cascades to every composite test. Here the two coexist,
    each fetches its OWN fields, and `to_regtype` resolves each name (bare,
    `schema.name`, and the quoted `"schema"."name"` a `sql.Identifier` renders)
    to the right oid. The bare name resolves only the public type; the schema
    name resolves only the schema-qualified one; `typname` stays unqualified.
    """
    from psycopg import sql
    from psycopg.types.composite import CompositeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # The whole fixture as one multi-statement simple query, as psycopg sends it.
        cur.execute(
            "create schema if not exists testschema;"
            " create type testcomp as (foo text, bar int8, baz float8);"
            " create type testschema.testcomp as (foo text, bar int8, qux bool);"
        )

        pub = CompositeInfo.fetch(conn, "testcomp")
        sch = CompositeInfo.fetch(conn, "testschema.testcomp")
        assert pub.name == "testcomp" and sch.name == "testcomp"  # typname is bare
        assert pub.oid != sch.oid  # distinct types
        assert list(pub.field_names) == ["foo", "bar", "baz"]
        assert list(sch.field_names) == ["foo", "bar", "qux"]
        assert list(pub.field_types) == [25, 20, 701]  # text, int8, float8
        assert list(sch.field_types) == [25, 20, 16]  # text, int8, bool

        # A quoted schema-qualified reference (what sql.Identifier renders) also
        # resolves to the schema-qualified type.
        ident = CompositeInfo.fetch(conn, sql.Identifier("testschema", "testcomp"))
        assert ident.oid == sch.oid

        # to_regtype: the bare name is the public type; the schema name is the
        # other; a bare name never reaches the schema-qualified type.
        cur.execute("select to_regtype('testcomp')::oid, to_regtype('testschema.testcomp')::oid")
        pub_oid, sch_oid = cur.fetchone()
        assert pub_oid == pub.oid
        assert sch_oid == sch.oid
        assert pub_oid != sch_oid

        # DROP is schema-aware: dropping the qualified type leaves the bare one.
        cur.execute("drop type testschema.testcomp")
        cur.execute("select to_regtype('testschema.testcomp'), to_regtype('testcomp')::oid")
        gone, still = cur.fetchone()
        assert gone is None
        assert still == pub.oid


def test_schema_qualified_range_is_distinct(home: Path) -> None:
    """`create type s.t as range` is a DISTINCT type from a bare `t`.

    psycopg's session-scoped range fixture creates `testschema.testrange` beside
    a bare `testrange` in one script; a server that resolved a qualified type
    name to its last part collided them and the second CREATE failed 42710,
    which -- the fixture being session-scoped -- cascaded to every range test.
    Here the two coexist, each carries its OWN subtype, and `to_regtype` resolves
    each name (bare, `schema.name`, and the quoted `"schema"."name"` a
    `sql.Identifier` renders) to the right oid. The bare name resolves only the
    public range; the schema name resolves only the schema-qualified one;
    `typname` stays unqualified (`testrange`), matching PostgreSQL.
    """
    from psycopg import sql
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # The whole fixture as one multi-statement simple query, as psycopg sends
        # it (subtypes chosen to differ so a collision could not go unnoticed).
        cur.execute(
            "create schema if not exists testschema;"
            ' create type testrange as range (subtype = text, collation = "C");'
            " create type testschema.testrange as range (subtype = float8);"
        )

        text_oid = conn.adapters.types["text"].oid
        float8_oid = conn.adapters.types["float8"].oid

        pub = RangeInfo.fetch(conn, "testrange")
        sch = RangeInfo.fetch(conn, "testschema.testrange")
        assert pub.name == "testrange" and sch.name == "testrange"  # typname is bare
        assert pub.oid != sch.oid  # distinct types
        assert pub.subtype_oid == text_oid
        assert sch.subtype_oid == float8_oid

        # A quoted schema-qualified reference (what sql.Identifier renders) also
        # resolves to the schema-qualified range.
        ident = RangeInfo.fetch(conn, sql.Identifier("testschema", "testrange"))
        assert ident.oid == sch.oid
        bare_ident = RangeInfo.fetch(conn, sql.Identifier("testrange"))
        assert bare_ident.oid == pub.oid

        # to_regtype: the bare name is the public range; the schema name is the
        # other; a bare name never reaches the schema-qualified range.
        cur.execute("select to_regtype('testrange')::oid, to_regtype('testschema.testrange')::oid")
        pub_oid, sch_oid = cur.fetchone()
        assert pub_oid == pub.oid
        assert sch_oid == sch.oid
        assert pub_oid != sch_oid

        # DROP is schema-aware: dropping the qualified type leaves the bare one.
        cur.execute("drop type testschema.testrange")
        cur.execute("select to_regtype('testschema.testrange'), to_regtype('testrange')::oid")
        gone, still = cur.fetchone()
        assert gone is None
        assert still == pub.oid


def test_custom_multirange_fetch_info(home: Path) -> None:
    """`MultirangeInfo.fetch` resolves a custom range's auto-created multirange.

    `CREATE TYPE testrange AS RANGE (...)` gives PostgreSQL a companion
    multirange type named by substituting `multirange` for the first `range`
    in the name (`testrange` -> `testmultirange`). psycopg's `MultirangeInfo.fetch`
    resolves that name with `to_regtype`, then joins `pg_type` to `pg_range` on
    `t.oid = r.rngmultitypid` -- so all three must exist: a resolvable multirange
    oid, a `pg_type` row under it (with its own array oid), and a `pg_range` row
    whose `rngmultitypid` points back at it, carrying the range's subtype. The
    schema-qualified range's multirange resolves distinctly, exactly like the
    range itself; `typname` stays the bare `testmultirange`, as in PostgreSQL.
    """
    from psycopg import sql
    from psycopg.types.multirange import MultirangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "create schema if not exists testschema;"
            ' create type testrange as range (subtype = text, collation = "C");'
            " create type testschema.testrange as range (subtype = float8);"
        )
        text_oid = conn.adapters.types["text"].oid
        float8_oid = conn.adapters.types["float8"].oid

        # The four forms psycopg's own test_fetch_info exercises: bare name,
        # schema.name, and each as a sql.Identifier.
        for name, subtype_oid in [
            ("testmultirange", text_oid),
            ("testschema.testmultirange", float8_oid),
            (sql.Identifier("testmultirange"), text_oid),
            (sql.Identifier("testschema", "testmultirange"), float8_oid),
        ]:
            info = MultirangeInfo.fetch(conn, name)
            assert info is not None, name
            assert info.name == "testmultirange"  # typname is bare
            assert info.oid > 0
            assert info.oid != info.array_oid > 0
            assert info.subtype_oid == subtype_oid

        # An unknown name resolves to None, not an error.
        assert MultirangeInfo.fetch(conn, "nosuchmultirange") is None


def test_schema_qualified_multirange_is_distinct(home: Path) -> None:
    """A public range's multirange and a `schema.` range's multirange differ.

    Both custom ranges yield a multirange whose bare `typname` is
    `testmultirange`, but they are distinct types with distinct oids and
    subtypes -- `to_regtype('testmultirange')` reaches only the public one and
    `to_regtype('testschema.testmultirange')` only the schema one, mirroring the
    range distinctness #1391 established.
    """
    from psycopg.types.multirange import MultirangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "create schema if not exists testschema;"
            " create type testrange as range (subtype = text);"
            " create type testschema.testrange as range (subtype = float8);"
        )
        pub = MultirangeInfo.fetch(conn, "testmultirange")
        sch = MultirangeInfo.fetch(conn, "testschema.testmultirange")
        assert pub.name == "testmultirange" and sch.name == "testmultirange"
        assert pub.oid != sch.oid
        assert pub.subtype_oid == conn.adapters.types["text"].oid
        assert sch.subtype_oid == conn.adapters.types["float8"].oid

        # to_regtype resolution keeps the two apart; a bare name never reaches
        # the schema-qualified multirange.
        cur.execute(
            "select to_regtype('testmultirange')::oid, to_regtype('testschema.testmultirange')::oid"
        )
        pub_oid, sch_oid = cur.fetchone()
        assert pub_oid == pub.oid
        assert sch_oid == sch.oid
        assert pub_oid != sch_oid


def test_a_plain_select_over_a_join(home: Path) -> None:
    """A top-level two-table JOIN outside any aggregate.

    The join source lives on `Select` as well as `Aggregate`; this is the
    non-grouped path RangeInfo.fetch takes.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select t.typname, r.rngsubtype from pg_type t "
            "join pg_range r on r.rngtypid = t.oid order by r.rngsubtype limit 2"
        )
        assert cur.fetchall() == [("int8range", 20), ("int4range", 23)]
        # A LEFT JOIN miss over the same tables yields the NULL side.
        cur.execute(
            "select t.typname, r.rngsubtype from pg_type t "
            "left join pg_range r on r.rngtypid = t.oid where t.oid = 25"
        )
        assert cur.fetchall() == [("text", None)]


def test_schema_ddl(home: Path) -> None:
    """CREATE SCHEMA / DROP SCHEMA, and a schema-qualified table.

    A duplicate is 42P06 (distinct from a table's 42P07 and a type's 42710), a
    missing DROP is 3F000, and CASCADE is accepted. Qualified names already
    resolve by their last part, so a table in a schema works once the schema
    DDL is allowed.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create schema testschema")
        assert cur.statusmessage == "CREATE SCHEMA"
        with pytest.raises(psycopg.errors.DuplicateSchema):
            cur.execute("create schema testschema")
        cur.execute("create schema if not exists testschema")
        cur.execute("drop schema if exists testschema cascade")
        with pytest.raises(psycopg.errors.InvalidSchemaName):
            cur.execute("drop schema testschema")
        cur.execute("drop schema if exists never")

        cur.execute("create schema s2")
        cur.execute("create table s2.t (id int4)")
        cur.execute("insert into s2.t values (1)")
        cur.execute("select id from s2.t")
        assert cur.fetchall() == [(1,)]
        cur.execute("drop schema s2 cascade")


def test_composite_type_ddl_and_catalog(home: Path) -> None:
    """CREATE TYPE ... AS (fields), DROP TYPE, and the catalog reads behind it.

    The DDL and the catalog (pg_type / to_regtype / regtype / pg_attribute) are
    the foundation CompositeInfo.fetch reads; the fetch query's nested-subquery
    join is a separate slice. A duplicate name is 42710 across composites,
    enums and builtins alike.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type testcomp as (foo text, bar int8, baz float8)")
        assert cur.statusmessage == "CREATE TYPE"
        with pytest.raises(psycopg.errors.DuplicateObject):
            cur.execute("create type testcomp as (x int4)")

        cur.execute("select typname, typrelid from pg_type where typname = 'testcomp'")
        name, typrelid = cur.fetchone()
        assert name == "testcomp"
        cur.execute("select oid from pg_type where typname = 'testcomp'")
        assert typrelid == cur.fetchone()[0]  # typrelid == its own oid
        cur.execute("select to_regtype('testcomp')::text")
        assert cur.fetchone()[0] == "testcomp"

        # pg_attribute exposes the fields, 1-based, with the element oids.
        cur.execute(
            "select attname, atttypid, attnum from pg_attribute"
            " where attrelid = to_regtype('testcomp') and attnum > 0"
            " and attisdropped = false order by attnum"
        )
        assert cur.fetchall() == [("foo", 25, 1), ("bar", 20, 2), ("baz", 701, 3)]

        cur.execute("drop type testcomp")
        cur.execute("select to_regtype('testcomp')")
        assert cur.fetchone()[0] is None


def test_a_composite_created_by_one_server_is_the_others_too(home: Path) -> None:
    """The composite catalog is a shared-store contract, minted like enums."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type rustcomp as (a int4, b text)")
        cur.execute("select oid from pg_type where typname = 'rustcomp'")
        oid = cur.fetchone()[0]
        assert oid >= 67000
    assert _python_sql(home, "SELECT to_regtype('rustcomp')::oid") == [(oid,)]
    _python_sql(home, "CREATE TYPE pycomp AS (c int8)")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select attname from pg_attribute where attrelid = to_regtype('pycomp')")
        assert cur.fetchall() == [("c",)]


def test_composite_value_record_cast(home: Path) -> None:
    """`'(1,x)'::testcomp` and `row(1,'x')::testcomp` build a composite VALUE.

    The record cast used to be an outright `FeatureNotSupported` (row form) or a
    mis-routed enum parse (`invalid input value for enum testcomp`, text form).
    Both now parse into a composite whose result column carries the composite's
    own oid, so psycopg's `register_composite` loader fires.
    """
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type testcomp as (foo int, bar text)")
        register_composite(CompositeInfo.fetch(conn, "testcomp"), conn)

        # Text-literal cast and record-constructor cast both round-trip, and the
        # loader turns them into the registered namedtuple.
        got = conn.execute("select '(1,x)'::testcomp").fetchone()[0]
        assert (got.foo, got.bar) == (1, "x")
        got = conn.execute("select row(2, 'y')::testcomp").fetchone()[0]
        assert (got.foo, got.bar) == (2, "y")


def test_composite_value_dump_and_table_round_trip(home: Path) -> None:
    """A composite param cast on the wire, and INSERT/SELECT through a column."""
    from psycopg.types.composite import CompositeInfo, register_composite

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type testcomp as (foo int, bar text)")
        info = CompositeInfo.fetch(conn, "testcomp")
        register_composite(info, conn)

        # Dump: a composite tuple param, explicitly cast on the wire.
        got = conn.execute("select %s::testcomp", [(7, "seven")]).fetchone()[0]
        assert (got.foo, got.bar) == (7, "seven")

        # Store it in a column of the composite type and read it back.
        cur.execute("create table ct (id int, val testcomp)")
        cur.execute("insert into ct values (1, %s)", [info.python_type(9, "nine")])
        got = conn.execute("select val from ct where id = 1").fetchone()[0]
        assert (got.foo, got.bar) == (9, "nine")


def test_composite_value_field_escaping(home: Path) -> None:
    """A composite VALUE renders its fields exactly as PostgreSQL does.

    NULL is empty, the empty string is `""`, and a field with a comma, quote,
    backslash, parenthesis or whitespace is double-quoted with `"`->`""` and
    `\\`->`\\\\`. The `::text` render is the surface a client compares against.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type tc AS (a int, b text)")
        cases = {
            "(,)": "(,)",
            '(1,"")': '(1,"")',
            '(1,"a,b")': '(1,"a,b")',
            '(1,"a""b")': '(1,"a""b")',
            '(1,"(x)")': '(1,"(x)")',
            '(1," sp ")': '(1," sp ")',
        }
        for literal, want in cases.items():
            got = cur.execute("select (%s::tc)::text", [literal]).fetchone()[0]
            assert got == want, (literal, got, want)


def test_composite_value_array_element_text(home: Path) -> None:
    """An array of composites renders each element as its `(...)` text.

    A record element used to leak Rust's `{:?}` debug form into the array
    literal (`{"Document({...})"}`); it now renders `{"(hello,10,30)"}`.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type tc AS (foo text, bar int8, baz float8)")
        got = cur.execute("select array[row('hello', 10, 30)::tc]").fetchone()[0]
        assert got == '{"(hello,10,30)"}'


def test_row_expressions_are_records(home: Path) -> None:
    """`ROW(...)` / `(a, b, ...)` build an anonymous record (oid 2249).

    psycopg decodes a record's text form to a tuple of strings. The text render
    follows PostgreSQL's composite rules -- a NULL field is empty, a field with
    a comma/quote/space is double-quoted, and a bool prints `t`/`f` -- and
    record comparison is three-valued (a NULL field before a decision is NULL).
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("SELECT ROW(1, 'a', true)")
        assert cur.fetchone()[0] == ("1", "a", "t")
        assert cur.description[0].type_code == 2249

        cur.execute("SELECT (1, 'a')")
        assert cur.fetchone()[0] == ("1", "a")

        # Text render with quoting and NULL-as-empty.
        cur.execute("SELECT ROW('a,b', 'c')::text, ROW(1, NULL, 3)::text, ROW('')::text")
        assert cur.fetchone() == ('("a,b",c)', "(1,,3)", '("")')

        # Comparison, including three-valued NULL.
        cur.execute("SELECT ROW(1, 2) = ROW(1, 2), ROW(1, 2) < ROW(1, 3)")
        assert cur.fetchone() == (True, True)
        cur.execute("SELECT ROW(1, NULL) = ROW(1, NULL), ROW(1, 2) < ROW(1, NULL)")
        assert cur.fetchone() == (None, None)
        # A decided inequality wins over a later NULL (= examines every pair).
        cur.execute("SELECT ROW(1, NULL, 3) = ROW(1, 2, 4)")
        assert cur.fetchone()[0] is False

        cur.execute("SELECT pg_typeof(ROW(1, 2))::text")
        assert cur.fetchone()[0] == "record"


def test_field_selection_from_record_and_composite(home: Path) -> None:
    """`(expr).field` selects a named field from a record or composite value.

    An anonymous record names its fields `f1`, `f2`, ... by position; a named
    composite carries its own field names, and the selected field reports the
    field's declared type in the row description (so `pg_typeof` reads it back).
    An unknown field is 42703; a field of a NULL composite is NULL.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        # Anonymous record: positional f1, f2, ...
        cur.execute("SELECT (ROW('a'::text, 'b'::text)).f1, (ROW('a'::text, 'b'::text)).f2")
        assert cur.fetchone() == ("a", "b")

        # Named composite: field by name, with the field's declared type.
        cur.execute("create type fc as (foo text, bar int8, baz float8)")
        cur.execute("SELECT (row('x', 42, 3.5)::fc).bar, (row('x', 42, 3.5)::fc).foo")
        assert cur.fetchone() == (42, "x")
        cur.execute("SELECT pg_typeof((row('x', 42, 3.5)::fc).bar)::text")
        assert cur.fetchone()[0] == "bigint"

        # A field of a NULL composite is NULL.
        cur.execute("SELECT (NULL::fc).bar")
        assert cur.fetchone()[0] is None

        # An unknown field is a 42703 UndefinedColumn, worded by the SOURCE
        # type: PostgreSQL names the composite for a named type and says
        # "record data type" for an anonymous row.
        with pytest.raises(psycopg.errors.UndefinedColumn) as exc:
            cur.execute("SELECT (row('x', 42, 3.5)::fc).nope").fetchone()
        assert exc.value.diag.message_primary == 'column "nope" not found in data type fc'
        # An anonymous record has exactly as many fN fields as values: a
        # position past the end, f0, and a non-positional name are all the
        # same error -- a NULL for `.f3` here was a silent divergence.
        for field in ("f3", "f0", "zz"):
            with pytest.raises(psycopg.errors.UndefinedColumn) as exc:
                cur.execute(f"SELECT (row(1, 'x')).{field}").fetchone()
            assert exc.value.diag.message_primary == (
                f'could not identify column "{field}" in record data type'
            )


def test_composite_value_equality_is_null_blind(home: Path) -> None:
    """Comparing composite VALUES treats NULL fields as equal (unlike a bare
    ROW constructor, which is three-valued).

    PostgreSQL: for composite-type values two NULL fields are equal and a NULL
    sorts larger than any non-NULL, so the comparison always resolves to
    true/false; a bare `ROW(...) = ROW(...)` with a NULL is still NULL. Nested
    composite fields compare by the same rule, recursively.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create type ce as (foo text, bar int8, baz float8)")
        cur.execute("create type ce2 as (qux int8, quux ce)")

        # Two composite values with a NULL field are EQUAL (not NULL).
        cur.execute(
            "SELECT row('foo', 1, NULL)::ce = row('foo', 1, NULL)::ce,"
            "       row('foo', 1, NULL)::ce <> row('foo', 1, NULL)::ce"
        )
        assert cur.fetchone() == (True, False)

        # A NULL beside a non-NULL is UNEQUAL, and NULL sorts larger.
        cur.execute(
            "SELECT row('foo', 1, NULL)::ce = row('foo', 1, 3.0)::ce,"
            "       row('foo', 1, NULL)::ce < row('foo', 1, 3.0)::ce"
        )
        assert cur.fetchone() == (False, False)

        # A decided inequality in a non-NULL field still wins.
        cur.execute("SELECT row('foo', 2, NULL)::ce = row('foo', 1, NULL)::ce")
        assert cur.fetchone()[0] is False

        # Nested composite fields compare by the same NULL-blind rule.
        cur.execute(
            "SELECT row(42, row('foo', 1, NULL)::ce)::ce2"
            "     = row(42, row('foo', 1, NULL)::ce)::ce2"
        )
        assert cur.fetchone()[0] is True

        # A bare ROW constructor stays three-valued: NULL, not True.
        cur.execute("SELECT ROW('foo', 1, NULL) = ROW('foo', 1, NULL)")
        assert cur.fetchone()[0] is None


def test_timestamptz_columns_render_in_session_zone(home: Path) -> None:
    """A `timestamptz` column stores a UTC INSTANT and renders in the session zone.

    The stored value is the same instant regardless of `SET timezone`; only its
    rendering moves. This needs the server to (a) store an instant, not
    session-rendered text, and (b) report the `TimeZone` GUC via ParameterStatus
    so psycopg re-expresses the instant in the session zone. A plain `timestamp`
    column stays naive (oid 1114).
    """
    import datetime as _dt

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table tz (id int primary key, t timestamptz)")
        cur.execute("insert into tz values (1, '2026-01-01 12:00:00+00')")
        cur.execute("insert into tz values (2, '2026-06-15 08:30:00.123456-04')")
        cur.execute("select t from tz where id = 1")
        assert cur.description[0].type_code == 1184

        # The same instant, rendered in three different session zones.
        expected = {
            "UTC": "2026-01-01T12:00:00+00:00",
            "Asia/Tokyo": "2026-01-01T21:00:00+09:00",
            "America/New_York": "2026-01-01T07:00:00-05:00",
        }
        for zone, iso in expected.items():
            cur.execute(f"set timezone = '{zone}'")
            cur.execute("select t from tz where id = 1")
            assert cur.fetchone()[0].isoformat() == iso, zone

        # Sub-millisecond precision survives the instant round-trip.
        cur.execute("set timezone = 'UTC'")
        cur.execute("select t from tz where id = 2")
        assert cur.fetchone()[0] == _dt.datetime(
            2026, 6, 15, 12, 30, 0, 123456, tzinfo=_dt.timezone.utc
        )

        # A plain `timestamp` column is unaffected -- naive, oid 1114.
        cur.execute("create table ts (id int primary key, t timestamp)")
        cur.execute("insert into ts values (1, '2026-01-01 12:00')")
        cur.execute("set timezone = 'Asia/Tokyo'")
        cur.execute("select t from ts")
        assert cur.description[0].type_code == 1114
        assert cur.fetchone()[0] == _dt.datetime(2026, 1, 1, 12, 0)


def test_custom_range_types(home: Path) -> None:
    """`CREATE TYPE ... AS RANGE (subtype = ...)` — a user range type.

    A user range has no canonical function, so `[1,4]` stays `[1,4]` (unlike the
    builtin int4range, which canonicalises to `[1,5)`). The type resolves as a
    regtype, casts parse/render over its subtype, RangeInfo.fetch finds its
    subtype via `pg_type JOIN pg_range`, and DROP TYPE removes it.
    """
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE myr AS RANGE (subtype = int4)")
        cur.execute("SELECT '[1,5)'::myr::text")
        assert cur.fetchone()[0] == "[1,5)"
        # No canonicalisation for a user range.
        cur.execute("SELECT '[1,4]'::myr::text")
        assert cur.fetchone()[0] == "[1,4]"
        cur.execute("SELECT 'empty'::myr::text")
        assert cur.fetchone()[0] == "empty"
        cur.execute("SELECT '(,5)'::myr::text")
        assert cur.fetchone()[0] == "(,5)"

        # RangeInfo.fetch resolves the subtype (compare the type NAME, since the
        # subtype oid is stable but the range oid is minted).
        info = RangeInfo.fetch(conn, "myr")
        cur.execute("SELECT %s::regtype::text", (info.subtype_oid,))
        assert cur.fetchone()[0] == "integer"

        # A numeric-subtype range too.
        cur.execute("CREATE TYPE mynr AS RANGE (subtype = numeric)")
        cur.execute("SELECT '[1.5,3.5)'::mynr::text")
        assert cur.fetchone()[0] == "[1.5,3.5)"

        # DROP removes it: the regtype no longer resolves.
        cur.execute("DROP TYPE myr")
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT 'myr'::regtype")
        assert exc.value.diag.sqlstate == "42704"


def test_binary_array_params_bytea_inet_uuid(home: Path) -> None:
    """bytea[] / inet[] / cidr[] / uuid[] round-trip as BINARY params and columns.

    psycopg sends these as binary array parameters (oids 1001 / 651 / 1041 /
    2951) and reads array columns back in binary; the element decode reuses the
    scalar bytea / inet / cidr / uuid decoders, and the array types report their
    own oid rather than falling through to varchar.

    Every column of a binary-requested row must actually COME BACK binary:
    psycopg decodes the whole row with the first column's format
    (`_py_transformer.py`, `fformat(0)`), so a row mixing a binary `uuid[]`
    with a text `inet[]` is undecodable ("unexpected number of dimensions").
    PG 16 honours the requested format on all eight columns here, reports oids
    `[1001, 2951, 1041, 651, 869, 650, 1041, 869]`, and renders the `::1`
    host address without its `/128` mask in text as well as binary.
    """
    import ipaddress
    import uuid as _uuid

    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table arr (b bytea[], u uuid[], n inet[], c cidr[], i inet, d cidr)")
        u1 = _uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")
        cur.execute(
            "insert into arr values (%b, %b, %b, %b, %b, %b)",
            (
                [b"\x01", b"\x02\xff"],
                [u1],
                [ipaddress.ip_interface("10.0.0.1/8"), ipaddress.ip_interface("::1")],
                [ipaddress.ip_network("10.0.0.0/8")],
                ipaddress.ip_interface("192.168.0.1/24"),
                ipaddress.ip_network("2001:db8::/32"),
            ),
        )
        for binary in (False, True):
            c = conn.cursor(binary=binary)
            c.execute("select b, u, n, c, i, d, array[null::inet], null::inet from arr")
            assert c.fetchone() == (
                [b"\x01", b"\x02\xff"],
                [u1],
                [ipaddress.ip_interface("10.0.0.1/8"), ipaddress.ip_address("::1")],
                [ipaddress.ip_network("10.0.0.0/8")],
                ipaddress.ip_interface("192.168.0.1/24"),
                ipaddress.ip_network("2001:db8::/32"),
                [None],
                None,
            ), binary
            assert [c.pgresult.fformat(k) for k in range(8)] == [int(binary)] * 8
            assert [d.type_code for d in c.description] == [
                1001,
                2951,
                1041,
                651,
                869,
                650,
                1041,
                869,
            ]


def test_join_multi_predicate_where(home: Path) -> None:
    """A JOIN's WHERE can be several ANDed predicates on either side.

    Foundation for composite `CompositeInfo.fetch` (whose inner subquery joins
    pg_attribute to pg_type with `WHERE t.oid=$1 AND a.attnum>0 AND NOT ...`).
    Verifies `=` plus a `>` on the join's right side over the catalog tables.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "SELECT t.typname FROM pg_type t JOIN pg_range r ON r.rngtypid = t.oid "
            "WHERE t.oid = 3904 AND r.rngsubtype > 0"
        )
        assert cur.fetchall() == [("int4range",)]


def test_user_types_are_visible_in_the_transaction_that_creates_them(home: Path) -> None:
    """A type created in a transaction is visible to later statements in it.

    Planning reads the type catalog OUTSIDE the open transaction (wrapping that
    read in the transaction deadlocks COPY), so an uncommitted CREATE TYPE was
    invisible and `CREATE TYPE t ...; SELECT 't'::regtype` in one transaction
    failed with 42704 -- every psycopg composite/enum/range fixture, whose conn
    is non-autocommit, hit it. An overlay consulted before the committed
    catalog fixes it, exactly as the table one does.
    """
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE ictx AS (a int, b text)")
        cur.execute("SELECT 'ictx'::regtype::text")
        assert cur.fetchone() == ("ictx",)
        cur.execute("CREATE TYPE ienum AS ENUM ('a', 'b')")
        cur.execute("SELECT 'ienum'::regtype::text")
        assert cur.fetchone() == ("ienum",)
        cur.execute("SELECT 'a'::ienum::text")
        assert cur.fetchone() == ("a",)
        cur.execute("CREATE TYPE irange AS RANGE (subtype = int4)")
        # The range RESOLVES in the transaction (no 42704). Its regtype text
        # renders the oid rather than the name -- a separate, pre-existing gap
        # that shows in autocommit too, tracked in the backlog -- so this
        # asserts only that the cast succeeds.
        cur.execute("SELECT 'irange'::regtype::text")
        assert cur.fetchone() is not None
        conn.commit()


def test_fetch_info_works_in_the_creating_transaction(home: Path) -> None:
    """psycopg's `*.fetch` helpers find a type created in the same transaction.

    This is the shape every composite/enum/range gauge fixture uses: on a
    non-autocommit connection, CREATE TYPE then `CompositeInfo.fetch` -- which
    returned None (`TypeError: no info passed`) while the type was invisible.
    """
    from psycopg.types.composite import CompositeInfo
    from psycopg.types.enum import EnumInfo
    from psycopg.types.range import RangeInfo

    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE ficomp AS (a int, b text)")
        info = CompositeInfo.fetch(conn, "ficomp")
        assert info is not None
        assert info.name == "ficomp"
        assert info.field_names == ("a", "b")
        cur.execute("CREATE TYPE fienum AS ENUM ('x', 'y')")
        einfo = EnumInfo.fetch(conn, "fienum")
        assert einfo is not None
        assert einfo.name == "fienum"
        assert einfo.labels == ["x", "y"]
        cur.execute("CREATE TYPE firange AS RANGE (subtype = int4)")
        rinfo = RangeInfo.fetch(conn, "firange")
        assert rinfo is not None
        assert rinfo.name == "firange"
        assert rinfo.subtype_oid == 23
        conn.rollback()


def test_rollback_discards_a_type_created_in_the_transaction(home: Path) -> None:
    """ROLLBACK removes an uncommitted CREATE TYPE, COMMIT keeps it."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("CREATE TYPE rb AS (x int)")
        cur.execute("SELECT 'rb'::regtype::text")
        assert cur.fetchone() == ("rb",)
        conn.rollback()
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("SELECT 'rb'::regtype::text")
        conn.rollback()
        # Committed this time, it persists into the next transaction.
        cur.execute("CREATE TYPE kept AS (x int)")
        conn.commit()
        cur.execute("SELECT 'kept'::regtype::text")
        assert cur.fetchone() == ("kept",)
        conn.commit()


def test_savepoint_rollback_discards_a_type_created_after_it(home: Path) -> None:
    """ROLLBACK TO undoes a CREATE TYPE issued after the savepoint."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("SAVEPOINT s1")
        cur.execute("CREATE TYPE sp AS (x int)")
        cur.execute("SELECT 'sp'::regtype::text")
        assert cur.fetchone() == ("sp",)
        cur.execute("ROLLBACK TO SAVEPOINT s1")
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("SELECT 'sp'::regtype::text")
        conn.rollback()
        # The rolled-back savepoint's type never persists.
        with pytest.raises(psycopg.errors.UndefinedObject):
            cur.execute("SELECT 'sp'::regtype::text")
        conn.rollback()


def test_non_holdable_cursor_is_invalid_after_commit(home: Path) -> None:
    """A `WITHOUT HOLD` cursor is closed by COMMIT; a `WITH HOLD` one survives.

    This is what psycopg's `ServerCursor(withhold=False)` relies on -- after
    `conn.commit()` a fetch must raise `InvalidCursorName` (34000). Silently
    keeping the cursor open would let a client read rows PostgreSQL discarded.
    """
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        # Default (no HOLD): gone after commit.
        cur.execute("declare c1 cursor for select * from generate_series(1, 3)")
        cur.execute("fetch 1 from c1")
        assert [r[0] for r in cur.fetchall()] == [1]
        conn.commit()
        with pytest.raises(psycopg.errors.InvalidCursorName):
            cur.execute("fetch 1 from c1")
        conn.rollback()
        # WITH HOLD: survives commit, keeps its position.
        cur.execute("declare c2 cursor with hold for select * from generate_series(1, 3)")
        cur.execute("fetch 1 from c2")
        assert [r[0] for r in cur.fetchall()] == [1]
        conn.commit()
        cur.execute("fetch 1 from c2")
        assert [r[0] for r in cur.fetchall()] == [2]
        cur.execute("close c2")


def test_rollback_closes_every_cursor_including_holdable(home: Path) -> None:
    """ROLLBACK closes ALL cursors, `WITH HOLD` included."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("declare h cursor with hold for select * from generate_series(1, 3)")
        cur.execute("fetch 1 from h")
        assert [r[0] for r in cur.fetchall()] == [1]
        conn.rollback()
        with pytest.raises(psycopg.errors.InvalidCursorName):
            cur.execute("fetch 1 from h")
        conn.rollback()


def test_no_scroll_cursor_rejects_a_backward_fetch(home: Path) -> None:
    """A `NO SCROLL` cursor may only scan forward.

    A backward FETCH/MOVE raises `ObjectNotInPrerequisiteState` (55000), while a
    plain (scrollable) cursor still scrolls both ways.
    """
    with _Server(home) as server, server.connect() as conn:
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare ns no scroll cursor for select * from generate_series(0, 5)")
            cur.execute("move 5 from ns")
            with pytest.raises(psycopg.errors.ObjectNotInPrerequisiteState):
                cur.execute("move backward 1 from ns")
        # A plain cursor is scrollable and still walks backward: MOVE 5 lands on
        # the row valued 4, and FETCH BACKWARD 1 then returns the one before it.
        with conn.transaction():
            cur = conn.cursor()
            cur.execute("declare ok cursor for select * from generate_series(0, 5)")
            cur.execute("move 5 from ok")
            cur.execute("fetch backward 1 from ok")
            assert [r[0] for r in cur.fetchall()] == [3]


def test_binary_server_cursor_returns_binary_rows(home: Path) -> None:
    """A binary server cursor's FETCH re-encodes rows in the binary wire format.

    The cursor's rows are frozen in TEXT at DECLARE, but psycopg requests BINARY
    on the FETCH -- so the server must re-encode from the captured typed values.
    A text server cursor over the same query still returns text.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.cursor().execute("create table tb (x int4)")
        conn.cursor().execute("insert into tb values (1), (2)")
        with conn.transaction():
            cur = conn.cursor("bc", binary=True)
            cur.execute("select x from tb order by x")
            assert cur.fetchone() == (1,)
            assert cur.pgresult.fformat(0) == 1
            assert cur.pgresult.get_value(0, 0) == b"\x00\x00\x00\x01"
            assert cur.fetchone() == (2,)
            assert cur.pgresult.get_value(0, 0) == b"\x00\x00\x00\x02"
        with conn.transaction():
            tcur = conn.cursor("tc")  # text cursor
            tcur.execute("select x from tb order by x")
            assert tcur.fetchone() == (1,)
            assert tcur.pgresult.fformat(0) == 0
            assert tcur.pgresult.get_value(0, 0) == b"1"


def test_generate_series_with_a_cast_in_the_target_list(home: Path) -> None:
    """`select generate_series(...)::type` casts each generated value.

    The cast rides as an ordinary per-row cast, and the described column type is
    the cast's -- so a binary server cursor over it decodes against the right
    oid. A constant WHERE over the series filters it (PG 16.15: `where false`
    is no rows, `where true` all three).
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select generate_series(1, 3)::int8 as n")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]
        assert cur.description[0].name == "n"
        assert cur.description[0].type_code == conn.adapters.types["int8"].oid
        cur.execute("select generate_series(1, 2)::text")
        assert [r[0] for r in cur.fetchall()] == ["1", "2"]
        cur.execute("select generate_series(1, 3) where false")
        assert cur.fetchall() == []
        cur.execute("select generate_series(1, 3) where true")
        assert [r[0] for r in cur.fetchall()] == [1, 2, 3]


def test_pg_backend_pid_returns_the_connections_pid(home: Path) -> None:
    """`pg_backend_pid()` reports the same PID the startup BackendKeyData did.

    The server only knows the PID pgwire assigned during startup, so the
    function is resolved from connection state rather than the stateless
    planner. Probed against PG 14, where the two always agree.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select pg_backend_pid()")
        (pid,) = cur.fetchone()
        assert pid == conn.pgconn.backend_pid
        assert isinstance(pid, int)


def test_pg_terminate_backend_on_self_breaks_the_connection(home: Path) -> None:
    """`pg_terminate_backend(pg_backend_pid())` ends the connection with a
    57P01, exactly as a real backend torn down by an administrator does. The
    connection must read as CLOSED afterwards -- the simple protocol carries no
    ReadyForQuery on a FATAL, so the client learns the socket is gone. Probed
    against PG 14."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.OperationalError) as exc:
            cur.execute("select pg_terminate_backend(pg_backend_pid())")
        assert exc.value.sqlstate == "57P01"
        assert isinstance(exc.value, psycopg.errors.AdminShutdown)
        assert conn.closed


def test_pg_terminate_backend_with_a_bound_pid_parameter(home: Path) -> None:
    """The same self-termination through the extended protocol: the PID is a
    bound parameter rather than a nested call. psycopg raises AdminShutdown and
    the connection breaks. Probed against PG 14."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.errors.AdminShutdown):
            cur.execute("select pg_terminate_backend(%s)", [conn.pgconn.backend_pid])
        assert conn.closed


def test_pg_terminate_backend_across_connections(home: Path) -> None:
    """One connection terminates ANOTHER by PID. The victim notices at its next
    statement and ends with 57P01; terminating a PID that is not a live backend
    returns false. Probed against PG 14."""
    with _Server(home) as server, server.connect() as victim, server.connect() as killer:
        victim_pid = victim.pgconn.backend_pid
        kcur = killer.cursor()
        kcur.execute("select pg_terminate_backend(%s)", [victim_pid])
        assert kcur.fetchone() == (True,)
        # The victim finds out on its next statement.
        with pytest.raises(psycopg.OperationalError) as exc:
            victim.execute("select 1")
        assert exc.value.sqlstate == "57P01"
        # A PID nobody is using is not a backend -> false.
        kcur.execute("select pg_terminate_backend(2147483)")
        assert kcur.fetchone() == (False,)


def test_error_inside_transaction_poisons_the_block(home: Path) -> None:
    """A syntax error inside a transaction aborts the block: every later
    statement gets 25P02 until COMMIT/ROLLBACK, and the COMMIT of a failed
    block rolls back. This holds whether the failing statement fails while the
    simple protocol SPLITS it or while the extended protocol DESCRIBES it.
    Probed against PG 14."""
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute("create table foo (id int primary key)")
        cur.execute("begin")
        cur.execute("insert into foo values (1)")
        with pytest.raises(psycopg.errors.SyntaxError):
            cur.execute("meh")
        # The block is now aborted: a perfectly valid statement is refused.
        with pytest.raises(psycopg.errors.InFailedSqlTransaction) as exc:
            cur.execute("select 1")
        assert exc.value.sqlstate == "25P02"
        # COMMIT of a failed block discards the write.
        cur.execute("commit")
        cur.execute("select count(*) from foo")
        assert cur.fetchone() == (0,)


def test_array_literal_keeps_unicode_whitespace_elements(home: Path) -> None:
    """The array scanner trims only PostgreSQL's `array_isspace` set (space,
    tab, newline, CR, VT, FF) -- never U+0085 / U+00A0, which Rust's
    `char::is_whitespace` strips. `select %s::text[]` with `['\\x85']` used
    to come back as the EMPTY array. Probed against PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for fmt in ("s", "t", "b"):
            for ch in ("\x85", "\xa0", " a "):
                cur.execute(f"select %{fmt}::text[]", ([ch],))
                assert cur.fetchone() == ([ch],), (fmt, ch)
        cur.execute("select '{ a , b }'::text[]")
        assert cur.fetchone() == (["a", "b"],)
        cur.execute("select E'{\\ta\\t,\\nb\\v\\f}'::text[]")
        assert cur.fetchone() == (["a", "b"],)
        cur.execute("select E'{\\u0085a}'::text[]")
        assert cur.fetchone() == (["\x85a"],)
        cur.execute('select \'{a b, "c d", " e ", NULL, "null"}\'::text[]')
        assert cur.fetchone() == (["a b", "c d", " e ", None, "null"],)
        cur.execute("select '{ { NULL } }'::text[]")
        assert cur.fetchone() == ([[None]],)


def test_array_literal_grammar_matches_postgresql(home: Path) -> None:
    """The literal forms PostgreSQL accepts and rejects (probed against PG 16):
    unquoted `NULL` is the null element while `"null"` is a string, a
    backslash escapes in and out of quotes, and each malformed shape is 22P02."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cases = {
            "{a b}": ["a b"],
            '{ NULL , "null", NuLl}': [None, "null", None],
            '{a\\,b, \\a, "a\\"b"}': ["a,b", "a", 'a"b'],
            "{}": [],
        }
        for literal, expected in cases.items():
            cur.execute("select %s::text[]", (literal,))
            assert cur.fetchone() == (expected,), literal
        for bad in ("{a,}", "{,a}", "{{}}", "{a}x", '{"a"b}', "{a", "a}", "{{a},{b,c}}", "{{a},b}"):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc:
                cur.execute("select %s::text[]", (bad,))
            assert exc.value.sqlstate == "22P02", bad


def test_array_literal_dimension_decoration(home: Path) -> None:
    """`[lo:hi]...={...}` is accepted, checked against the contents, and
    otherwise discarded -- the value has no lower bounds. Probed against PG
    16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select '[0:1]={a,b}'::text[]")
        assert cur.fetchone() == (["a", "b"],)
        cur.execute("select '[1:1][-2:-1][3:5]={{{1,2,3},{4,5,6}}}'::int[]")
        assert cur.fetchone() == ([[[1, 2, 3], [4, 5, 6]]],)
        cur.execute("select '[0:1]={a,b}'::text[] || %s::text[]", (["c"],))
        assert cur.fetchone() == (["a", "b", "c"],)
        for bad in ("[0:0]={a,b}", "[1:2={a,b}", "[1:2]{a,b}"):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation):
                cur.execute("select %s::text[]", (bad,))


NESTED_TEXT = [[["fo{o", "ba}r"], ['ba"z', "qu'x"], ["qu ux", " "]]]


def test_nested_arrays_round_trip_in_every_format(home: Path) -> None:
    """A multidimensional array parses to nested lists from its text literal,
    from a text parameter, and from a BINARY parameter (whose decoder used to
    refuse `ndim > 1`), and renders back the same way. Probed against PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("select '{{{{{{\"NULL\"}}}}}}'::text[]")
        assert cur.fetchone() == ([[[[[["NULL"]]]]]],)
        for fmt in ("s", "t", "b"):
            cur.execute(f"select %{fmt}::text[]", (NESTED_TEXT,))
            assert cur.fetchone() == (NESTED_TEXT,), fmt
            cur.execute(f"select %{fmt}::int[]", ([[1, 2], [3, 4]],))
            assert cur.fetchone() == ([[1, 2], [3, 4]],), fmt
            cur.execute(f"select %{fmt}::text[]", ([[[[[["NULL"]]]]]],))
            assert cur.fetchone() == ([[[[[["NULL"]]]]]],), fmt
        bcur = conn.cursor(binary=True)
        bcur.execute("select %b::int[]", ([[1, None], [3, 4]],))
        assert bcur.fetchone() == ([[1, None], [3, 4]],)


def test_array_concatenation_follows_array_cat(home: Path) -> None:
    """`||` on arrays is `array_cat` / `array_append` / `array_prepend`: same
    dimensionality appends, an (N-1)-dim side becomes a new slice, a NULL side
    yields the other, and the result is typed as the array. Probed against PG
    16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cases = [
            ("select array[1] || null", [1]),
            ("select null || array[1]", [1]),
            ("select array[1] || 2", [1, 2]),
            ("select 0 || array[1]", [0, 1]),
            ("select array[[1,2]] || array[3,4]", [[1, 2], [3, 4]]),
            ("select array[3,4] || array[[1,2]]", [[3, 4], [1, 2]]),
            ("select array[[1,2]] || array[[3,4]]", [[1, 2], [3, 4]]),
            ("select '{}'::int[] || array[1]", [1]),
            ("select '{{1,2},{3,4}}'::int[] || '{5,6}'", [[1, 2], [3, 4], [5, 6]]),
        ]
        for sql, expected in cases:
            cur.execute(sql)
            assert cur.fetchone() == (expected,), sql
        cur.execute("select pg_typeof(array[1] || 2)")
        assert cur.fetchone() == ("integer[]",)
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            cur.execute("select array['a'] || 'b'")


def test_expressions_over_generate_series_rows(home: Path) -> None:
    """A select-list expression over a `generate_series` row -- arithmetic, a
    cast, a scalar call, date arithmetic, a comparison -- evaluates per row,
    is named as PostgreSQL names it, and is typed from the row's declared
    column type. Every value, name and `pg_typeof` here is PG 16's."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select i, i + 1, i::int8 as j, i * 2.5, abs(-i), "
            "'2021-01-01'::date + i, i > 1 from generate_series(1, 2) as i"
        )
        assert [d.name for d in cur.description] == [
            "i",
            "?column?",
            "j",
            "?column?",
            "abs",
            "?column?",
            "?column?",
        ]
        assert cur.fetchall() == [
            (1, 2, 1, Decimal("2.5"), 1, dt.date(2021, 1, 2), False),
            (2, 3, 2, Decimal("5.0"), 2, dt.date(2021, 1, 3), True),
        ]
        cur.execute(
            "select pg_typeof(abs(-i)), pg_typeof(i + 1), pg_typeof(i * 2.5), "
            "pg_typeof(i::int8), pg_typeof('2021-01-01'::date + i), "
            "pg_typeof(i > 1), pg_typeof(i) from generate_series(1, 1) as i"
        )
        assert cur.fetchone() == (
            "integer",
            "integer",
            "numeric",
            "bigint",
            "date",
            "boolean",
            "integer",
        )
        # The stream path (Execute with a row limit) takes the same route.
        rows = list(
            cur.stream(
                "select i, '2021-01-01'::date + i from generate_series(1, %s) as i",
                [2],
            )
        )
        assert rows == [(1, dt.date(2021, 1, 2)), (2, dt.date(2021, 1, 3))]


def test_describe_distinguishes_no_columns_from_no_rows(home: Path) -> None:
    """Describe answers `NoData` only for a statement that returns no rows at
    all; a `select` with an empty target list is a RowDescription of zero
    fields and yields one empty row. psycopg's `stream()` reads the difference:
    a NoData select raised `the last operation didn't produce a result`.
    Probed against PG 16 at the protocol level."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        assert list(cur.stream("select")) == [()]
        assert cur.description == []
        cur.execute("create table if not exists nodata_t (a int)")
        assert cur.description is None
        assert list(cur.stream("select %s::int", [1], binary=True)) == [(1,)]
        with conn.cursor(binary=True) as bcur:
            assert list(bcur.stream("select %s::int", [1])) == [(1,)]


def test_serial_columns_and_insert_returning(home: Path) -> None:
    """`serial` / `bigserial` draw from a `<table>_<column>_seq` sequence, an
    empty bound list binds as an empty array, and `RETURNING` answers named,
    computed and `*` columns typed from the row. Every value here is PG 16's
    (a fresh store, so the first id is 1)."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table fp_test (id serial primary key, data date[])")
        for fmt in ("s", "t", "b"):
            cur.execute(f"insert into fp_test (data) values (%{fmt}) returning id", ([],))
            assert (cur.rowcount, cur.statusmessage) == (1, "INSERT 0 1")
            assert [d.type_code for d in cur.description] == [23]
        assert cur.fetchone() == (3,)
        cur.execute("insert into fp_test (data) values (%s), (%s)", (["2021-01-01"], []))
        assert (cur.rowcount, cur.statusmessage) == (2, "INSERT 0 2")
        cur.execute("select id, data from fp_test order by id")
        assert cur.fetchall() == [
            (1, []),
            (2, []),
            (3, []),
            (4, [dt.date(2021, 1, 1)]),
            (5, []),
        ]
        cur.execute("select data from fp_test where id = any(%s)", ([1],))
        assert cur.fetchall() == [([],)]
        cur.execute("select data from fp_test where id = any(%s)", ([],))
        assert cur.fetchall() == []
        # An explicit id neither draws from nor advances the sequence.
        cur.execute("insert into fp_test (id, data) values (100, null) returning id")
        assert cur.fetchone() == (100,)
        cur.execute(
            "insert into fp_test (data) values (null) "
            "returning id, data, id * 2, pg_typeof(id)::text"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("id", 23),
            ("data", 1182),
            ("?column?", 23),
            ("pg_typeof", 25),
        ]
        assert cur.fetchone() == (6, None, 12, "integer")
        cur.execute("insert into fp_test (data) values (null) returning *")
        assert [d.name for d in cur.description] == ["id", "data"]
        assert cur.fetchone() == (7, None)
        # A cast of a column keeps the column's name; a cast of an expression
        # is named after the type.
        cur.execute("insert into fp_test (data) values (null) returning id::int8, (1+2)::int8")
        assert [(d.name, d.type_code) for d in cur.description] == [("id", 20), ("int8", 20)]
        assert cur.fetchone() == (8, 3)
        with pytest.raises(psycopg.errors.UndefinedColumn) as ei:
            cur.execute("insert into fp_test (data) values (null) returning nope")
        assert str(ei.value).startswith('column "nope" does not exist')

        cur.execute("create table fp_big (id bigserial, n int)")
        cur.execute("insert into fp_big (n) values (1) returning id, pg_typeof(id)::text")
        assert [d.type_code for d in cur.description] == [20, 25]
        assert cur.fetchone() == (1, "bigint")
        cur.execute("insert into fp_big (n) values (2), (3) returning id")
        assert cur.fetchall() == [(2,), (3,)]
        cur.executemany(
            "insert into fp_big (n) values (%s) returning n, id",
            [(10,), (20,)],
            returning=True,
        )
        assert (cur.rowcount, cur.statusmessage, cur.fetchone()) == (1, "INSERT 0 1", (10, 4))
        assert cur.nextset() is True
        assert (cur.rowcount, cur.fetchone(), cur.nextset()) == (1, (20, 5), None)

        # Dropping the table drops its sequence: a re-created table starts
        # over at 1.
        cur.execute("drop table fp_test")
        cur.execute("create table fp_test (id serial primary key, data date[])")
        cur.execute("insert into fp_test (data) values (null) returning id")
        assert cur.fetchone() == (1,)


def test_show_and_insert_select_rowcounts(home: Path) -> None:
    """`SHOW` completes with a bare `SHOW` tag and one row; `INSERT ... SELECT`
    inserts the query's rows (it used to write nothing and say `INSERT 0 0`),
    with PostgreSQL's width errors and default-filled trailing columns.
    Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("show timezone")
        assert (cur.rowcount, cur.statusmessage) == (1, "SHOW")
        assert [(d.name, d.type_code) for d in cur.description] == [("TimeZone", 25)]
        cur.execute("create table fp_trc (id int primary key, n int default 7)")
        assert (cur.rowcount, cur.statusmessage) == (-1, "CREATE TABLE")
        cur.execute("insert into fp_trc select generate_series(1, 42)")
        assert (cur.rowcount, cur.statusmessage) == (42, "INSERT 0 42")
        cur.execute("select count(*), min(n), max(n) from fp_trc")
        assert cur.fetchone() == (42, 7, 7)
        cur.execute(
            "insert into fp_trc (id, n) select i * 100, i + 1 "
            "from generate_series(1, 3) as i returning id, n"
        )
        assert (cur.rowcount, cur.statusmessage) == (3, "INSERT 0 3")
        assert cur.fetchall() == [(100, 2), (200, 3), (300, 4)]
        cur.execute("insert into fp_trc select 700 where false")
        assert (cur.rowcount, cur.statusmessage) == (0, "INSERT 0 0")
        with pytest.raises(psycopg.errors.UniqueViolation):
            cur.execute("insert into fp_trc select 1")
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("insert into fp_trc select 'a'")
        assert str(ei.value).startswith('invalid input syntax for type integer: "a"')
        for sql in (
            "insert into fp_trc select 200, 1, 2",
            "insert into fp_trc (id) select 300, 1",
            "insert into fp_trc values (600, 1, 2)",
        ):
            with pytest.raises(psycopg.errors.SyntaxError) as ei:
                cur.execute(sql)
            assert str(ei.value).startswith("INSERT has more expressions than target columns")
        for sql in (
            "insert into fp_trc (id, n) select 400",
            "insert into fp_trc (id, n) values (500)",
        ):
            with pytest.raises(psycopg.errors.SyntaxError) as ei:
                cur.execute(sql)
            assert str(ei.value).startswith("INSERT has more target columns than expressions")
        # COPY of a query evaluates its expressions rather than reading the
        # bare columns.
        with cur.copy(
            "copy (select id + 1, n::text || 'x' from fp_trc where id <= 2 order by id) to stdout"
        ) as cp:
            assert b"".join(cp) == b"2\t7x\n3\t7x\n"


def test_literal_column_defaults(home: Path) -> None:
    """A literal DEFAULT is applied to every column an INSERT omits, whatever
    the INSERT's shape; an explicit NULL is kept; a DEFAULT that will not
    parse as the column's type fails at CREATE. An expression default is
    refused (0A000) rather than silently dropped. Values are PG 16's."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "create table fp_d (id int primary key, n int default 7, t text default 'x', "
            "d date default '2021-01-02', z int default null, f float8 default 1.5, "
            "b bool default true)"
        )
        cur.execute("insert into fp_d (id) values (1)")
        cur.execute("insert into fp_d values (2)")
        cur.execute("insert into fp_d (id, n, t) values (3, null, 'y')")
        cur.execute("insert into fp_d select 4")
        cur.execute("insert into fp_d (id, t) select 5, 'q' returning *")
        assert cur.fetchone() == (5, 7, "q", dt.date(2021, 1, 2), None, 1.5, True)
        cur.execute("select * from fp_d order by id")
        assert cur.fetchall() == [
            (1, 7, "x", dt.date(2021, 1, 2), None, 1.5, True),
            (2, 7, "x", dt.date(2021, 1, 2), None, 1.5, True),
            (3, None, "y", dt.date(2021, 1, 2), None, 1.5, True),
            (4, 7, "x", dt.date(2021, 1, 2), None, 1.5, True),
            (5, 7, "q", dt.date(2021, 1, 2), None, 1.5, True),
        ]
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("create table fp_e (id int, n int default 'a')")
        assert str(ei.value).startswith('invalid input syntax for type integer: "a"')
        with pytest.raises(psycopg.errors.FeatureNotSupported):
            cur.execute("create table fp_e (id int, n text default now())")
    # The default survives a restart: it is in the catalog, not the session.
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("insert into fp_d (id) values (6) returning n, t")
        assert cur.fetchone() == (7, "x")


def test_constant_where_on_a_from_less_select(home: Path) -> None:
    """With no FROM, a WHERE is a constant predicate on the one row: false or
    NULL is zero rows (the predicate used to be ignored), a non-boolean is
    42804, and an unknown literal is read as a boolean. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for sql, args in (
            ("select 1 where false", None),
            ("select 1 where null", None),
            ("select 1 where %s", (False,)),
            ("select %s where %s", (5, False)),
            ("select 1 where 1 < 2 and false", None),
        ):
            cur.execute(sql, args)
            assert (cur.fetchall(), cur.statusmessage) == ([], "SELECT 0"), sql
        for sql, args, row in (
            ("select 1, 'a' where true", None, (1, "a")),
            ("select 1 where 1 = 1", None, (1,)),
            ("select 1 where %s", (True,), (1,)),
            ("select 1 where 't'", None, (1,)),
        ):
            cur.execute(sql, args)
            assert (cur.fetchall(), cur.statusmessage) == ([row], "SELECT 1"), sql
        with pytest.raises(psycopg.errors.DatatypeMismatch) as ei:
            cur.execute("select 1 where 1")
        assert str(ei.value).startswith("argument of WHERE must be type boolean, not type integer")
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select 1 where 'x'")
        assert str(ei.value).startswith('invalid input syntax for type boolean: "x"')


def test_pg_sleep_waits_and_returns_void(home: Path) -> None:
    """`pg_sleep(seconds)` waits on the connection's thread and answers a
    `void` (oid 2278) column rendered as the empty string in both formats;
    zero or negative seconds return at once, NULL is NULL, and a bad literal
    is 22P02 for double precision. psycopg's `test_executemany_lock` needs
    it. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        started = time.monotonic()
        cur.execute("select pg_sleep(0.2)")
        assert time.monotonic() - started >= 0.2
        assert [(d.name, d.type_code) for d in cur.description] == [("pg_sleep", 2278)]
        assert (cur.fetchall(), cur.statusmessage) == ([("",)], "SELECT 1")
        cur.execute("select pg_sleep(%s)", (0,))
        assert cur.fetchall() == [("",)]
        cur.execute("select pg_sleep(-1), 1")
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("pg_sleep", 2278),
            ("?column?", 23),
        ]
        assert cur.fetchall() == [("", 1)]
        cur.execute("select pg_sleep(null)")
        assert cur.fetchall() == [(None,)]
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select pg_sleep('x')")
        assert str(ei.value).startswith('invalid input syntax for type double precision: "x"')
        with conn.cursor(binary=True) as bcur:
            bcur.execute("select pg_sleep(0)")
            assert bcur.fetchall() == [(b"",)]
        # A false WHERE drops the row without waiting.
        started = time.monotonic()
        cur.execute("select pg_sleep(5) where false")
        assert cur.fetchall() == []
        assert time.monotonic() - started < 1


def test_crossed_range_inside_an_array_literal_is_a_data_error(home: Path) -> None:
    """A range element whose bounds cross is 22000 from the range parser, not
    a malformed-array 22P02: the array literal's structure is fine. Probed
    PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        with pytest.raises(psycopg.errors.DataException) as ei:
            cur.execute("select '{\"[5,1]\"}'::int4range[]")
        assert str(ei.value).startswith(
            "range lower bound must be less than or equal to range upper bound"
        )


def test_session_user_value_functions(home: Path) -> None:
    """`user` / `current_user` / `session_user` / `current_role` answer the
    role the client connected as (this fixture connects as `test`),
    `current_catalog` / `current_schema` are `postgres` / `public`, every
    column is typed `name` (oid 19) and named after its keyword, and the
    QUOTED form `"user"` is an ordinary (missing) column. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select user, current_user, session_user, current_catalog, current_schema, current_role"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("user", 19),
            ("current_user", 19),
            ("session_user", 19),
            ("current_catalog", 19),
            ("current_schema", 19),
            ("current_role", 19),
        ]
        assert cur.fetchall() == [("test", "test", "test", "postgres", "public", "test")]
        cur.execute("select user as u")
        assert [(d.name, d.type_code) for d in cur.description] == [("u", 19)]
        assert cur.fetchall() == [("test",)]
        with pytest.raises(psycopg.errors.UndefinedColumn) as ei:
            cur.execute('select "user"')
        assert str(ei.value).startswith('column "user" does not exist')


def test_box_type_casts_and_arrays(home: Path) -> None:
    """`box` (oid 603, array 1020): every input spelling normalises to
    `(high),(low)` with the corners re-ordered per coordinate, a NaN lands in
    the high corner, `box[]` is delimited by `;` rather than `,` (so its
    elements are never quoted), a text-typed array of boxes IS quoted, a
    bound text parameter casts, and the errors are PostgreSQL's: 22P02 for
    a malformed box, 22003 for a coordinate out of float8 range, 42846 for
    the casts PostgreSQL does not have. Every value probed on PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select '(1,2),(3,4)'::box, '((1,2),(3,4))'::box, '1,2,3,4'::box, "
            "'(1,4),(3,2)'::box, '(1.5,2),(3,4e2)'::box, '(2,3),(nan,1)'::box, "
            "'(1,2),(3,4)'::box::text, pg_typeof('(1,2),(3,4)'::box)"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("box", 603),
            ("text", 25),
            ("pg_typeof", 2206),
        ]
        assert cur.fetchall() == [
            (
                "(3,4),(1,2)",
                "(3,4),(1,2)",
                "(3,4),(1,2)",
                "(3,4),(1,2)",
                "(3,400),(1.5,2)",
                "(NaN,3),(2,1)",
                "(3,4),(1,2)",
                "box",
            )
        ]
        cur.execute(
            "select array['(1,2),(3,4)'::box, '(5,6),(7,8)'::box], "
            "array['(1,2),(3,4)'::box, null], "
            "'{(1,2),(3,4);(5,6),(7,8)}'::box[], '{\"(1,2),(3,4)\"}'::box[], "
            "array['(1,2),(3,4)'::box]::text[], pg_typeof(array['(1,2),(3,4)'::box])"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("array", 1020),
            ("array", 1020),
            ("box", 1020),
            ("box", 1020),
            ("array", 1009),
            ("pg_typeof", 2206),
        ]
        assert cur.fetchall() == [
            (
                ["(3,4),(1,2)", "(7,8),(5,6)"],
                ["(3,4),(1,2)", None],
                ["(3,4),(1,2)", "(7,8),(5,6)"],
                ["(3,4),(1,2)"],
                ["(3,4),(1,2)"],
                "box[]",
            )
        ]
        # The raw wire text of a box[] uses the `;` delimiter, unquoted.
        cur.execute("select '{(1,2),(3,4);(5,6),(7,8)}'::box[]::text")
        assert cur.fetchall() == [("{(3,4),(1,2);(7,8),(5,6)}",)]
        cur.execute("select %s::box", ("(1,2),(3,4)",))
        assert [(d.name, d.type_code) for d in cur.description] == [("box", 603)]
        assert cur.fetchall() == [("(3,4),(1,2)",)]
        # psycopg dumps a text list with `,` -- which is not box[]'s
        # delimiter -- so the literal is malformed, on PostgreSQL too.
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select %s::box[]", (["(1,2),(3,4)", "(5,6),(7,8)"],))
        assert str(ei.value).startswith('malformed array literal: "{"(1,2),(3,4)","(5,6),(7,8)"}"')
        for bad in ("(1,2),(3)", "(1,2),(3,4),(5,6)", "x", "(1,2)", "((1,2),(3,4)"):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
                cur.execute("select %s::box", (bad,))
            assert str(ei.value).startswith(f'invalid input syntax for type box: "{bad}"')
        with pytest.raises(psycopg.errors.NumericValueOutOfRange) as ei:
            cur.execute("select '(1e400,2),(3,4)'::box")
        assert str(ei.value).startswith('"1e400" is out of range for type double precision')
        with pytest.raises(psycopg.errors.CannotCoerce) as ei:
            cur.execute("select 5::box")
        assert str(ei.value).startswith("cannot cast type integer to box")
        with pytest.raises(psycopg.errors.CannotCoerce) as ei:
            cur.execute("select '(1,2),(3,4)'::box::float8")
        assert str(ei.value).startswith("cannot cast type box to double precision")


def test_float8_text_is_float8out(home: Path) -> None:
    """A double's text form is PostgreSQL's `float8out` -- shortest round-trip
    digits, exponent form outside 1e-4..1e15 with a two-digit signed
    exponent, `Infinity` / `-Infinity` / `NaN`, a signed `-0` -- in a column,
    in a cast and inside a `float8[]`. A `float8` with a numeric operand is
    float8 arithmetic, unary minus keeps a double's signed zero, and numeric
    has no negative zero at all. Every value probed on PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select 1e20::float8::text, 1e-7::float8::text, 1e15::float8::text, "
            "1e14::float8::text, 0.00001::float8::text, 0.0001::float8::text, "
            "'inf'::float8::text, '-inf'::float8::text, 'nan'::float8::text, "
            "1.5::float8::text, 2::float8::text, 123456789012345678::float8::text, "
            "(0.1::float8 + 0.2)::text"
        )
        assert cur.fetchall() == [
            (
                "1e+20",
                "1e-07",
                "1e+15",
                "100000000000000",
                "1e-05",
                "0.0001",
                "Infinity",
                "-Infinity",
                "NaN",
                "1.5",
                "2",
                "1.2345678901234568e+17",
                "0.30000000000000004",
            )
        ]
        cur.execute(
            "select array[1e20::float8, 0.5]::text, array[1.5::float8, 2]::text, "
            "array[1e20::float8, 0.5], array[1.5::float8, 2]"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("array", 25),
            ("array", 25),
            ("array", 1022),
            ("array", 1022),
        ]
        assert cur.fetchall() == [("{1e+20,0.5}", "{1.5,2}", [1e20, 0.5], [1.5, 2.0])]
        cur.execute(
            "select 0.1::float8 + 0.2, 1.5::numeric + 1::float8, 2::float8 * 1.5, 1.5 / 2::float8"
        )
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("?column?", 701),
            ("?column?", 701),
            ("?column?", 701),
            ("?column?", 701),
        ]
        assert cur.fetchall() == [(0.30000000000000004, 2.5, 3.0, 0.75)]
        cur.execute(
            "select (-(0.0::float8))::text, (-0.0::float8)::text, ('-0'::float8)::text, "
            "(- 0.0)::text, (-0.00e3)::text, (0.0 - 0.0)::text, 0.00e3::text"
        )
        assert cur.fetchall() == [("-0", "-0", "-0", "0.0", "0", "0.0", "0")]


def test_constant_select_columns_are_named_as_postgresql_names_them(home: Path) -> None:
    """A FROM-less select names an unaliased cast after its target type's
    catalog name (`int4`, `float8`, `bpchar`, `numeric`, `char`), a nested
    cast after the OUTER type, the constructor keywords after themselves
    (`array`, `row`, `coalesce`, `greatest`, `least`, `nullif`), a cast of a
    call after the call, and everything else `?column?`. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select array[1], row(1), coalesce(1), greatest(1,2), least(1), "
            "('1'::text)::int8, -1::int, 1 + 1, nullif(1,2), not true, "
            "true and false, 1 = any(array[1]), (1), ((1::int)), "
            "1::double precision, 'a'::char(2), 1::decimal, '1'::\"char\", "
            "cast(1 as int), abs(-1)::int8"
        )
        assert [d.name for d in cur.description] == [
            "array",
            "row",
            "coalesce",
            "greatest",
            "least",
            "int8",
            "?column?",
            "?column?",
            "nullif",
            "?column?",
            "?column?",
            "?column?",
            "?column?",
            "int4",
            "float8",
            "bpchar",
            "numeric",
            "char",
            "int4",
            "abs",
        ]


def test_boolean_casts_exist_only_for_integer_and_text(home: Path) -> None:
    """`boolean` has casts from `integer` and text and to `integer`; every
    other numeric, temporal or integer-width pairing is 42846 `cannot cast
    type X to Y` (a `1.5::bool` used to be a 22P02 parse failure). The source
    type is what decides -- `'2021-01-01'::bool` is the 22P02 text failure
    while `'2021-01-01'::date::bool` is 42846 -- and a NULL casts through the
    same rules. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute(
            "select 1::bool, 0::bool, 2::bool, 'yes'::bool, 't'::bool::int, "
            "1::bool::int, false::int, true::int4"
        )
        assert cur.fetchone() == (True, False, True, True, 1, 1, 0, 1)
        assert [d.type_code for d in cur.description] == [16, 16, 16, 16, 23, 23, 23, 23]
        for sql, message in [
            ("select 1.5::bool", "cannot cast type numeric to boolean"),
            ("select 1::int8::bool", "cannot cast type bigint to boolean"),
            ("select 1::int2::bool", "cannot cast type smallint to boolean"),
            ("select 1.5::float8::bool", "cannot cast type double precision to boolean"),
            ("select '2021-01-01'::date::bool", "cannot cast type date to boolean"),
            ("select null::date::bool", "cannot cast type date to boolean"),
            ("select true::int8", "cannot cast type boolean to bigint"),
            ("select true::int2", "cannot cast type boolean to smallint"),
        ]:
            with pytest.raises(psycopg.errors.CannotCoerce) as ei:
                cur.execute(sql)
            assert str(ei.value).startswith(message), sql
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            cur.execute("select '2021-01-01'::bool")
        assert str(ei.value).startswith('invalid input syntax for type boolean: "2021-01-01"')


def test_pg_prepared_statements_lists_protocol_prepared_statements(home: Path) -> None:
    """`pg_prepared_statements` shows the connection's NAMED protocol-level
    statements with PG's columns: `parameter_types` as regtype display names,
    `result_types` NULL for a statement that returns no rows, `from_sql`
    false, and a statement without parameters counted as one generic plan
    where a parameterised one counts as one custom plan (psycopg executes
    once at prepare time). The unnamed statement never appears. Probed PG 16
    with psycopg 3.3 / libpq 18."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table pp (id serial primary key, num int, s text, j jsonb)")
        cur.execute("insert into pp (num, s) values (1, 'a')")
        cur.execute("select count(*) from pg_prepared_statements")
        assert cur.fetchone() == (0,)
        cur.execute("select num + %s::smallint, s || %s from pp", (1, "x"), prepare=True)
        cur.execute("update pp set num = %s", (5,), prepare=True)
        cur.execute(
            "insert into pp (j) values (%s)", (psycopg.types.json.Jsonb({"a": 1}),), prepare=True
        )
        cur.execute("select 1", prepare=True)
        cur.execute("select 2", prepare=False)
        cur.execute(
            "select name, statement, parameter_types, result_types, from_sql, "
            "generic_plans, custom_plans from pg_prepared_statements order by name"
        )
        assert cur.fetchall() == [
            (
                "_pg3_0",
                "select num + $1::smallint, s || $2 from pp",
                ["smallint", "text"],
                ["integer", "text"],
                False,
                0,
                1,
            ),
            ("_pg3_1", "update pp set num = $1", ["smallint"], None, False, 0, 1),
            ("_pg3_2", "insert into pp (j) values ($1)", ["jsonb"], None, False, 0, 1),
            ("_pg3_3", "select 1", [], ["integer"], False, 1, 0),
        ]
        cur.execute("select * from pg_prepared_statements")
        assert [(d.name, d.type_code) for d in cur.description] == [
            ("name", 25),
            ("statement", 25),
            ("prepare_time", 1184),
            ("parameter_types", 2211),
            ("result_types", 2211),
            ("from_sql", 16),
            ("generic_plans", 20),
            ("custom_plans", 20),
        ]
        cur.execute("select prepare_time from pg_prepared_statements limit 1")
        (prepared_at,) = cur.fetchone()
        assert prepared_at.tzinfo is not None
        # DEALLOCATE of one name, then of all; a name the session does not
        # hold is 26000.
        cur.execute("deallocate _pg3_1")
        cur.execute("select name from pg_prepared_statements order by name")
        assert cur.fetchall() == [("_pg3_0",), ("_pg3_2",), ("_pg3_3",)]
        with pytest.raises(psycopg.errors.InvalidSqlStatementName) as ei:
            cur.execute("deallocate nosuch")
        assert str(ei.value).startswith('prepared statement "nosuch" does not exist')
        cur.execute("deallocate all")
        cur.execute("select count(*) from pg_prepared_statements")
        assert cur.fetchone() == (0,)
        cur.execute("notify foo")
        assert cur.statusmessage == "NOTIFY"


def test_protocol_close_removes_a_prepared_statement(home: Path) -> None:
    """psycopg evicts a prepared statement past `prepared_max` with the
    protocol `Close` message (libpq 18's `PQclosePrepared`), and the row
    leaves `pg_prepared_statements` with it. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        conn.prepare_threshold = 0
        conn.prepared_max = 1
        for i in range(3):
            conn.execute(f"select {i}")
        listing = "select name, statement from pg_prepared_statements order by name"
        assert conn.execute(listing).fetchall() == [("_pg3_2", "select 2"), ("_pg3_3", listing)]
        conn.execute("select 100")
        assert conn.execute(listing).fetchall() == [("_pg3_4", "select 100"), ("_pg3_5", listing)]


def test_uuid_text_input_forms(home: Path) -> None:
    """`uuid_in` accepts the 32-hex form, braces, upper case, and hyphens
    after ANY group of four hex digits -- not only at the canonical
    positions -- and rejects a short string, a non-hex digit, and leading
    whitespace with 22P02. A text parameter bound to a `$n::uuid` is
    canonicalised on the way in. Probed PG 16."""
    canonical = uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")
    with _Server(home) as server, server.connect() as conn:
        for text in [
            "a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}",
            "a0eebc99-9c0b4ef8-bb6d6bb9-bd380a11",
            "a0ee-bc99-9c0b-4ef8-bb6d-6bb9-bd38-0a11",
            "{a0eebc99-9c0b4ef8-bb6d6bb9-bd380a11}",
            "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11",
        ]:
            assert conn.execute("select %s::uuid", (text,)).fetchone() == (canonical,), text
        for text in [
            "a0eebc999c0b4ef8bb6d6bb9bd380a1",
            "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a1z",
            " a0eebc999c0b4ef8bb6d6bb9bd380a11",
        ]:
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
                conn.execute("select %s::uuid", (text,))
            assert str(ei.value).startswith(f'invalid input syntax for type uuid: "{text}"')
        assert conn.execute(
            "select %s::uuid::text", ("a0eebc999c0b4ef8bb6d6bb9bd380a11",)
        ).fetchone() == (str(canonical),)
        assert conn.execute("select %s", (canonical,)).fetchone() == (canonical,)


def test_bytea_and_uuid_results_are_sent_binary_when_asked(home: Path) -> None:
    """A binary-format request gets `bytea`, `uuid`, and their arrays in
    binary (uuid as its 16 raw bytes), including NULL elements and a NULL
    value; `set_byte` answers bytea. Probed PG 16."""
    u = uuid.UUID("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        cur.execute(
            "select 'abc'::bytea, %s::uuid, array[%s::uuid, null], "
            "array['ab'::bytea, null], null::uuid, set_byte('abc'::bytea, 0, 100)",
            (str(u), str(u)),
        )
        assert cur.fetchone() == (b"abc", u, [u, None], [b"ab", None], None, b"dbc")
        assert [d.type_code for d in cur.description] == [17, 2950, 2951, 1001, 2950, 17]
        assert cur.pgresult is not None
        assert cur.pgresult.fformat(0) == 1
        # An untyped text parameter compared with a text expression.
        cur.execute("select %s = chr(%s)", ("a", 97))
        assert cur.fetchone() == (True,)
        cur.execute("select %s = 'a'::text", ("a",))
        assert cur.fetchone() == (True,)


def test_nul_byte_in_a_binary_text_parameter_is_22021(home: Path) -> None:
    """A binary-format text parameter carrying a NUL byte is refused with
    22021 `invalid byte sequence for encoding "UTF8": 0x00` (a text-format
    one never reaches the server: psycopg refuses it client-side). Probed
    PG 16."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.CharacterNotInRepertoire) as ei:
            conn.execute("select %b::text", ("a\x00b",))
        assert str(ei.value).startswith('invalid byte sequence for encoding "UTF8": 0x00')


def test_any_typed_functions_refuse_an_untyped_parameter(home: Path) -> None:
    """A function whose arguments are `any` / VARIADIC `any` cannot resolve
    a bare parameter: 42P18 `could not determine data type of parameter
    $n`, naming the FIRST unresolvable one -- `format`'s and `concat_ws`'s
    leading `text` argument resolves, so they fault `$2`. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        for call, faulted in [
            ("concat(%s, %s)", 1),
            ("concat_ws(%s, %s)", 2),
            ("format(%s, %s)", 2),
            ("num_nulls(%s, %s)", 1),
            ("num_nonnulls(%s, %s)", 1),
            ("json_build_array(%s, %s)", 1),
            ("jsonb_build_object(%s, %s)", 1),
        ]:
            with pytest.raises(psycopg.errors.IndeterminateDatatype) as ei:
                conn.execute(f"select {call}", ("a", "b"))
            assert str(ei.value).startswith(
                f"could not determine data type of parameter ${faulted}"
            ), call
        assert conn.execute("select concat_ws(%s, 'a', 'b')", ("x",)).fetchone() == ("axb",)


def test_copy_out_encoding_error_keeps_the_connection(home: Path) -> None:
    """A COPY TO STDOUT that fails mid-stream (a character the client
    encoding cannot represent) surfaces its 22P05 and leaves the connection
    usable and idle -- the server used to follow the error with a CopyFail,
    after which the client saw "you cannot mix COPY with other operations"
    on the next statement. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table enc (s text)")
        conn.execute("insert into enc values ('€')")
        conn.execute("set client_encoding to latin1")
        with (
            pytest.raises(psycopg.errors.UntranslatableCharacter) as ei,
            conn.cursor().copy("copy enc to stdout") as cp,
        ):
            list(cp)
        assert str(ei.value).startswith(
            'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" '
            'has no equivalent in encoding "LATIN1"'
        )
        assert conn.execute("select 1").fetchone() == (1,)
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE


def test_update_set_with_a_row_expression(home: Path) -> None:
    """`UPDATE ... SET col = <expression over the row>` -- `num * 2`,
    `upper(s)`, `s || 'x'`, `coalesce(num, 0)`, `num::text`, a bound
    parameter in the expression -- evaluates per matched row from the row's
    pre-update values, and an unknown column is 42703. Probed PG 16."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("create table ut (id serial primary key, num int, s text)")
        cur.execute("insert into ut (num, s) values (3, 'a'), (4, 'b'), (null, 'c')")
        rows = "select id, num, s from ut order by id"
        cur.execute("update ut set num = num * 2")
        assert cur.statusmessage == "UPDATE 3"
        assert cur.execute(rows).fetchall() == [(1, 6, "a"), (2, 8, "b"), (3, None, "c")]
        cur.execute("update ut set s = upper(s), num = num + 1 where num > 6")
        assert cur.statusmessage == "UPDATE 1"
        assert cur.execute(rows).fetchall() == [(1, 6, "a"), (2, 9, "B"), (3, None, "c")]
        cur.execute("update ut set num = num + %s where id = %s", (10, 1))
        assert cur.statusmessage == "UPDATE 1"
        assert cur.execute(rows).fetchall() == [(1, 16, "a"), (2, 9, "B"), (3, None, "c")]
        cur.execute("update ut set s = s || 'x', num = coalesce(num, 0)")
        assert cur.statusmessage == "UPDATE 3"
        assert cur.execute(rows).fetchall() == [(1, 16, "ax"), (2, 9, "Bx"), (3, 0, "cx")]
        cur.execute("update ut set s = num::text")
        assert cur.execute("select id, s from ut order by id").fetchall() == [
            (1, "16"),
            (2, "9"),
            (3, "0"),
        ]
        with pytest.raises(psycopg.errors.UndefinedColumn) as ei:
            cur.execute("update ut set num = nosuch * 2")
        assert str(ei.value).startswith('column "nosuch" does not exist')


def test_savepoint_rollback_of_ddl_on_a_fresh_store(home: Path) -> None:
    """On a store with no committed tables yet, ROLLBACK TO a savepoint
    still undoes a CREATE TYPE issued after it (the restore used to read the
    catalog through a session blind to the transaction's own writes, and
    left the type behind: `type ... already exists` on the retry); a table
    created before the savepoint survives while the rows written after it
    are undone. Probed PG 16."""
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        cur.execute("savepoint s1")
        cur.execute("create type prepenum as enum ('foo', 'bar')")
        cur.execute("rollback to savepoint s1")
        cur.execute("create type prepenum as enum ('foo', 'bar')")
        cur.execute("select 'prepenum'::regtype::text")
        assert cur.fetchone() == ("prepenum",)
        conn.rollback()
        cur.execute("create table sp_t (id serial primary key, n int)")
        cur.execute("savepoint s2")
        cur.execute("insert into sp_t (n) values (1)")
        cur.execute("rollback to savepoint s2")
        cur.execute("select count(*) from sp_t")
        assert cur.fetchone() == (0,)
        conn.commit()
        cur.execute("select count(*) from sp_t")
        assert cur.fetchone() == (0,)
        conn.commit()


# ---------------------------------------------------------------------------
# DO blocks, error diagnostics, numeric / oid / enum / record wire paths.
# Every expected value below was measured on PostgreSQL 16.15 (2026-09-09).
# ---------------------------------------------------------------------------


def test_unary_minus_on_numeric_specials(home: Path) -> None:
    """`-'NaN'::numeric` is NaN, and the infinities flip sign."""
    with _Server(home) as server, server.connect() as conn:
        row = conn.execute(
            "select -'NaN'::numeric, -'Infinity'::numeric, -'-Infinity'::numeric, -(1.5::numeric)"
        ).fetchone()
        assert row is not None
        assert row[0].is_nan()
        assert row[1:] == (Decimal("-Infinity"), Decimal("Infinity"), Decimal("-1.5"))


def test_oid_parameters_in_text_and_binary(home: Path) -> None:
    """An `Oid` parameter keeps its type (`pg_typeof` is `oid`, the column
    oid is 26), the full unsigned range round-trips, and an `oid[]` sent as
    text comes back binary as a 1028 array."""
    from psycopg.types.numeric import Oid

    with _Server(home) as server, server.connect() as conn:
        cur = conn.execute("select pg_typeof(%s)::text, %s", [Oid(10), Oid(4294967295)])
        assert cur.fetchone() == ("oid", 4294967295)
        assert cur.description is not None
        assert [d.type_code for d in cur.description] == [25, 26]
        cur = conn.cursor(binary=True)
        cur.execute("select pg_typeof(%s)::text, %s", [Oid(10), Oid(7)])
        assert cur.fetchone() == ("oid", 7)
        cur.execute("select %s, pg_typeof(%s)::text", [[Oid(1), Oid(2)], [Oid(1), Oid(2)]])
        assert cur.fetchone() == ([1, 2], "oid[]")
        assert cur.description is not None
        assert cur.description[0].type_code == 1028


def test_where_over_generate_series(home: Path) -> None:
    """A constant or column predicate filters the generated rows; a
    non-boolean constant is `42804`."""
    with _Server(home) as server, server.connect() as conn:
        assert conn.execute("select 1 from generate_series(1,3) where false").fetchall() == []
        assert conn.execute("select 1 from generate_series(1,3) where true").fetchall() == [
            (1,),
            (1,),
            (1,),
        ]
        assert conn.execute(
            "select count(*) from generate_series(1,5) i where i > 2"
        ).fetchone() == (3,)
        assert conn.execute(
            "select i from generate_series(1,5) i where i > 2 order by i desc"
        ).fetchall() == [(5,), (4,), (3,)]
        with pytest.raises(psycopg.errors.DatatypeMismatch) as ei:
            conn.execute("select 1 from generate_series(1,3) where 1")
        assert str(ei.value).startswith("argument of WHERE must be type boolean, not type integer")


def test_quote_ident_format_and_boolean_concat(home: Path) -> None:
    """`quote_ident` quotes reserved keywords (`order`, `select`) and leaves
    unreserved ones (`int4`, `abort`, `zone`) bare; `format()` handles
    `%s` / `%I` / `%L` / `%%` / `%2$s`, NULL arguments, and its three error
    shapes; a boolean's text inside `concat` is `t` / `f`."""
    with _Server(home) as server, server.connect() as conn:
        assert conn.execute(
            "select quote_ident('order'), quote_ident('select'), quote_ident('int4'),"
            " quote_ident('abort'), quote_ident('zone'), quote_ident('Foo'),"
            " quote_ident('a\"b'), quote_ident('a-b'), quote_ident('1a'), quote_ident('_ok')"
        ).fetchone() == (
            '"order"',
            '"select"',
            "int4",
            "abort",
            "zone",
            '"Foo"',
            '"a""b"',
            '"a-b"',
            '"1a"',
            "_ok",
        )
        assert conn.execute(
            "select format('%s|%I|%L|%%|%2$s', 'x', 'order', 'it''s'),"
            " format('%s-%L-%I', null, null, 'a'), format(null, 1), format('%L', E'a\\\\b')"
        ).fetchone() == ("x|\"order\"|'it''s'|%|order", "-NULL-a", None, "E'a\\\\b'")
        with pytest.raises(psycopg.errors.NullValueNotAllowed) as ei:
            conn.execute("select format('%I', null)")
        assert str(ei.value).startswith("null values cannot be formatted as an SQL identifier")
        with pytest.raises(psycopg.errors.InvalidParameterValue) as ei2:
            conn.execute("select format('%s %s', 1)")
        assert str(ei2.value).startswith("too few arguments for format()")
        with pytest.raises(psycopg.errors.InvalidParameterValue) as ei3:
            conn.execute("select format('%x', 1)")
        assert str(ei3.value).startswith('unrecognized format() type specifier "x"')
        assert ei3.value.diag.message_hint == 'For a single "%" use "%%".'
        assert conn.execute("select concat(true, false, 1, 1.5, 'x'), true::text").fetchone() == (
            "tf11.5x",
            "true",
        )


def test_regtype_quotes_a_reserved_keyword_type_name(home: Path) -> None:
    """A type named `order` renders as `"order"` through `regtype`."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute('create type "order" as (x int)')
        assert conn.execute("select '\"order\"'::regtype::text").fetchone() == ('"order"',)


def test_undefined_table_error_carries_its_position(home: Path) -> None:
    """`42P01` points `P` at the relation's first mention, counted in
    characters across lines (`select 1 +\\n 2 from nope` is 20)."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.UndefinedTable) as ei:
            conn.execute("select * from wat")
        assert ei.value.diag.statement_position == "15"
        assert ei.value.diag.severity_nonlocalized == "ERROR"
        with pytest.raises(psycopg.errors.UndefinedTable) as ei2:
            conn.execute("select 1 +\n 2 from nope")
        assert ei2.value.diag.statement_position == "20"


def _notices(conn: psycopg.Connection) -> list[tuple[str, str | None, str, str | None]]:
    seen: list[tuple[str, str | None, str, str | None]] = []
    conn.add_notice_handler(
        lambda d: seen.append(
            (d.severity or "", d.severity_nonlocalized, d.message_primary or "", d.context)
        )
    )
    return seen


def test_do_block_raise_levels_and_notices(home: Path) -> None:
    """`RAISE NOTICE / WARNING / INFO` reach the client as notices with the
    block's context; `DEBUG` and `LOG` do not (below `client_min_messages`).
    Both `DO $$ ... $$ LANGUAGE plpgsql` spellings work, as does `NULL;`."""
    with _Server(home) as server, server.connect() as conn:
        seen = _notices(conn)
        conn.execute(
            "do $$ begin raise notice 'n %', 1; raise warning 'w'; raise debug 'd';"
            " raise info 'i'; raise log 'l'; end $$"
        )
        ctx = "PL/pgSQL function inline_code_block line 1 at RAISE"
        assert seen == [
            ("NOTICE", "NOTICE", "n 1", ctx),
            ("WARNING", "WARNING", "w", ctx),
            ("INFO", "INFO", "i", ctx),
        ]
        seen.clear()
        conn.execute("do $$ begin raise notice 'x'; end $$ language plpgsql")
        conn.execute("do language plpgsql $$ begin raise notice 'y'; end $$")
        assert [n[2] for n in seen] == ["x", "y"]
        conn.execute("do $$ begin null; end $$")
        assert conn.execute("select 1").fetchone() == (1,)


def test_do_block_raise_exception_diagnostics(home: Path) -> None:
    """`RAISE EXCEPTION` carries the message, `USING` fields, the sqlstate
    (default `P0001`, a named condition, or an arbitrary `errcode`), and the
    block context; a non-ASCII message survives."""
    ctx = "PL/pgSQL function inline_code_block line 1 at RAISE"
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.DivisionByZero) as ei:
            conn.execute(
                "do $$ begin raise exception 'boom %', 'x'"
                " using errcode = '22012', detail = 'd', hint = 'h'; end $$"
            )
        d = ei.value.diag
        assert (d.message_primary, d.message_detail, d.message_hint, d.context) == (
            "boom x",
            "d",
            "h",
            ctx,
        )
        assert d.severity_nonlocalized == "ERROR"
        with pytest.raises(psycopg.errors.RaiseException) as ei2:
            conn.execute("do $$ begin raise exception 'boom'; end $$")
        assert (ei2.value.sqlstate, ei2.value.diag.message_primary) == ("P0001", "boom")
        with pytest.raises(psycopg.errors.DivisionByZero) as ei3:
            conn.execute("do $$ begin raise division_by_zero; end $$")
        assert ei3.value.diag.message_primary == "division_by_zero"
        with pytest.raises(psycopg.InternalError) as ei4:
            conn.execute("do $$ begin raise exception 'boom' using errcode = 'XX123'; end $$")
        assert ei4.value.sqlstate == "XX123"
        with pytest.raises(psycopg.errors.RaiseException) as ei5:
            conn.execute("do $$ begin raise exception 'bad é'; end $$")
        assert ei5.value.diag.message_primary == "bad é"
        conn.execute("set client_encoding to latin9")
        with pytest.raises(psycopg.errors.RaiseException) as ei6:
            conn.execute("do $$ begin raise exception 'bad €'; end $$")
        assert ei6.value.diag.message_primary == "bad €"
        assert conn.execute("select 'bad €'").fetchone() == ("bad €",)


def test_do_block_perform_and_execute_contexts(home: Path) -> None:
    """An error inside `PERFORM` stacks the SQL statement under the block
    frame; one inside `EXECUTE` carries the executed query and its position
    in the `q` / `p` fields instead."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.DivisionByZero) as ei:
            conn.execute("do $$ begin perform 1/0; end $$")
        assert ei.value.diag.message_primary == "division by zero"
        assert ei.value.diag.context == (
            'SQL statement "SELECT 1/0"\nPL/pgSQL function inline_code_block line 1 at PERFORM'
        )
        with pytest.raises(psycopg.errors.UndefinedTable) as ei2:
            conn.execute("do $$ begin execute 'select * from nope'; end $$")
        d = ei2.value.diag
        assert d.context == "PL/pgSQL function inline_code_block line 1 at EXECUTE"
        assert (d.internal_query, d.internal_position) == ("select * from nope", "15")
        assert d.statement_position is None


def test_do_block_compile_and_condition_errors(home: Path) -> None:
    """A bad body is `42601` positioned inside the statement; an unknown
    condition NAME fails at compile time with the compilation context, while
    an unknown `errcode` fails at the RAISE; a non-plpgsql language is
    `0A000`."""
    with _Server(home) as server, server.connect() as conn:
        with pytest.raises(psycopg.errors.SyntaxError) as ei:
            conn.execute("do $$ begin raise notice 'x' end $$")
        assert str(ei.value).startswith('syntax error at or near "end"')
        assert ei.value.diag.statement_position == "30"
        with pytest.raises(psycopg.errors.SyntaxError) as ei2:
            conn.execute("do $$ raise notice 'x'; $$")
        assert str(ei2.value).startswith('syntax error at or near "raise"')
        assert ei2.value.diag.statement_position == "7"
        with pytest.raises(psycopg.errors.UndefinedObject) as ei3:
            conn.execute("do $$ begin raise unknown_thing; end $$")
        assert str(ei3.value).startswith('unrecognized exception condition "unknown_thing"')
        assert ei3.value.diag.context == (
            'compilation of PL/pgSQL function "inline_code_block" near line 1'
        )
        with pytest.raises(psycopg.errors.UndefinedObject) as ei4:
            conn.execute("do $$ begin raise exception 'x' using errcode = 'unknown_thing'; end $$")
        assert ei4.value.diag.context == "PL/pgSQL function inline_code_block line 1 at RAISE"
        with pytest.raises(psycopg.errors.FeatureNotSupported) as ei5:
            conn.execute("do language sql $$ select 1 $$")
        assert str(ei5.value).startswith('language "sql" does not support inline code execution')


def test_copy_out_renders_inet_without_a_host_mask(home: Path) -> None:
    """`inet` drops a `/32` (`/128`) host mask on output and keeps any other;
    `cidr` always shows its mask; an IPv4-mapped address renders dotted."""
    import io

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cpi (a inet, b cidr)")
        conn.execute(
            "insert into cpi values ('127.0.0.1/32', '10.0.0.0/8'),"
            " ('::ffff:102:300/128', '::ffff:1.2.3.0/120'), ('192.168.0.1/24', '192.168.0.0/24')"
        )
        buf = io.StringIO()
        with conn.cursor().copy("copy cpi to stdout") as cp:
            for chunk in cp:
                buf.write(bytes(chunk).decode())
        assert buf.getvalue() == (
            "127.0.0.1\t10.0.0.0/8\n"
            "::ffff:1.2.3.0\t::ffff:1.2.3.0/120\n"
            "192.168.0.1/24\t192.168.0.0/24\n"
        )
        assert conn.execute("select a::text, b::text from cpi").fetchall() == [
            ("127.0.0.1/32", "10.0.0.0/8"),
            ("::ffff:1.2.3.0/128", "::ffff:1.2.3.0/120"),
            ("192.168.0.1/24", "192.168.0.0/24"),
        ]


def test_copy_in_keeps_numeric_text_exact(home: Path) -> None:
    """`COPY FROM` stores a numeric at its written scale: a tiny fraction,
    a 34-digit value, `-0.0` (which reads back `0.0`) and `NaN`."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table cpn (n numeric)")
        with conn.cursor().copy("copy cpn from stdin") as cp:
            cp.write("0.000000000000000001\n123456789012345678901234567890.1234\n-0.0\nNaN\n")
        assert conn.execute("select n::text from cpn").fetchall() == [
            ("0.000000000000000001",),
            ("123456789012345678901234567890.1234",),
            ("0.0",),
            ("NaN",),
        ]


def test_enum_parameters_labels_and_arrays(home: Path) -> None:
    """A parameter typed with the enum's oid is checked against its labels
    (`22P02` with the portal context); an enum ARRAY parameter parses in text
    and in binary, and a binary result carries the enum array's oid with
    each element as its label."""
    import enum

    from psycopg.adapt import Dumper
    from psycopg.types.enum import EnumInfo, register_enum

    with _Server(home) as server, server.connect() as conn:
        conn.execute("create type dnm_enum as enum ('ONE', 'TWO', 'THREE')")
        info = EnumInfo.fetch(conn, "dnm_enum")
        assert info is not None
        assert info.labels == ["ONE", "TWO", "THREE"]
        e = enum.Enum("E", {label: label for label in info.labels})
        register_enum(info, conn, e)
        assert conn.execute("select %s::text", [e.ONE]).fetchone() == ("ONE",)
        assert conn.execute("select %s::dnm_enum[]", [["ONE", "TWO"]]).fetchone() == (
            [e.ONE, e.TWO],
        )
        assert conn.execute("select %b::dnm_enum[]", [[e.ONE, e.TWO]]).fetchone() == (
            [e.ONE, e.TWO],
        )
        cur = conn.cursor(binary=True)
        cur.execute("select %s::dnm_enum[]", [[e.ONE, e.TWO]])
        assert cur.description is not None
        assert cur.description[0].type_code == info.array_oid
        assert cur.pgresult is not None
        raw = cur.pgresult.get_value(0, 0)
        assert raw is not None
        # ndim=1, no nulls, element oid, dim 2 lower-bound 1, then the labels.
        assert raw == (
            b"\x00\x00\x00\x01\x00\x00\x00\x00"
            + info.oid.to_bytes(4, "big")
            + b"\x00\x00\x00\x02\x00\x00\x00\x01"
            b"\x00\x00\x00\x03ONE\x00\x00\x00\x03TWO"
        )
        assert cur.fetchone() == ([e.ONE, e.TWO],)

        class WithEnumOid(Dumper):
            oid = info.oid

            def dump(self, obj: str) -> bytes:
                return obj.encode()

        conn.adapters.register_dumper(str, WithEnumOid)
        with pytest.raises(psycopg.errors.InvalidTextRepresentation) as ei:
            conn.execute("select %s::text", ["NOPE"])
        assert str(ei.value).startswith('invalid input value for enum dnm_enum: "NOPE"')
        assert ei.value.diag.context == "unnamed portal parameter $1 = '...'"


def test_anonymous_record_binary_result_carries_field_types(home: Path) -> None:
    """`ROW(...)` in binary: a 4-byte field count, then per field the oid and
    length-prefixed bytes (`-1` for NULL). An untyped literal is `unknown`
    (705), a cast one its type -- byte-identical to PostgreSQL 16.15."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor(binary=True)
        expected = {
            "select row()": (b"\x00\x00\x00\x00", ((),)),
            "select row(null)": (b"\x00\x00\x00\x01\x00\x00\x02\xc1\xff\xff\xff\xff", ((None,),)),
            "select row(null, '')": (
                b"\x00\x00\x00\x02\x00\x00\x02\xc1\xff\xff\xff\xff\x00\x00\x02\xc1\x00\x00\x00\x00",
                ((None, b""),),
            ),
            "select row(42, 'foo', 'ba,r')": (
                b"\x00\x00\x00\x03\x00\x00\x00\x17\x00\x00\x00\x04\x00\x00\x00*"
                b"\x00\x00\x02\xc1\x00\x00\x00\x03foo\x00\x00\x02\xc1\x00\x00\x00\x04ba,r",
                ((42, b"foo", b"ba,r"),),
            ),
            "select row(10::int, null::text, 20::float, null::text, 'foo'::text, 'bar'::bytea)": (
                b"\x00\x00\x00\x06\x00\x00\x00\x17\x00\x00\x00\x04\x00\x00\x00\n"
                b"\x00\x00\x00\x19\xff\xff\xff\xff\x00\x00\x02\xbd\x00\x00\x00\x08@4\x00\x00\x00\x00\x00\x00"
                b"\x00\x00\x00\x19\xff\xff\xff\xff\x00\x00\x00\x19\x00\x00\x00\x03foo"
                b"\x00\x00\x00\x11\x00\x00\x00\x03bar",
                ((10, None, 20.0, None, "foo", b"bar"),),
            ),
        }
        for query, (raw, row) in expected.items():
            cur.execute(query)
            assert cur.description is not None
            assert cur.description[0].type_code == 2249, query
            assert cur.pgresult is not None
            assert cur.pgresult.get_value(0, 0) == raw, query
            assert cur.fetchone() == row, query
        assert conn.execute("select row(42, 'foo', 'ba,r')").fetchone() == (("42", "foo", "ba,r"),)


def test_binary_results_cover_every_faker_type(home: Path) -> None:
    """Every type psycopg's faker generates has a BINARY result form.

    psycopg reads EVERY column of a result in the format of column 0
    (`Transformer.set_pgresult` looks only at `PQfformat(res, 0)`, because
    PostgreSQL never mixes formats in one reply), so one text-described column
    in an otherwise binary row made the client run text loaders over binary
    bytes -- `decimal.InvalidOperation`, garbage dates, `UnicodeDecodeError` --
    which is what failed `test_leak` / `test_copy_to_leaks` / `test_random`.
    The bytes below are PostgreSQL 16's (`scratchpad/binpin.py`), so this is
    byte fidelity, not just "psycopg could decode it".
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        cases: list[tuple[str, str, object]] = [
            ("'2000-01-02'::date", "00000001", dt.date(2000, 1, 2)),
            ("'12:00:00+05:30'::timetz", "0000000a0eebb000ffffb2a8", None),
            (
                "'2000-01-01 00:00:01'::timestamp",
                "00000000000f4240",
                dt.datetime(2000, 1, 1, 0, 0, 1),
            ),
            (
                "'2000-01-01 00:00:01+00'::timestamptz",
                "00000000000f4240",
                dt.datetime(2000, 1, 1, 0, 0, 1, tzinfo=dt.timezone.utc),
            ),
            (
                "'1 day 2 hours'::interval",
                "00000001ad2748000000000100000000",
                dt.timedelta(days=1, hours=2),
            ),
            ("'-1 mon -3 days'::interval", "0000000000000000fffffffdffffffff", None),
            (
                "'[2000-01-01,2000-01-02)'::tsrange",
                "0200000008000000000000000000000008000000141dd76000",
                Range(dt.datetime(2000, 1, 1), dt.datetime(2000, 1, 2), "[)"),
            ),
            (
                "'[-infinity,infinity]'::tsrange",
                "06000000088000000000000000000000087fffffffffffffff",
                None,
            ),
            ("'empty'::int4range", "01", Range(empty=True)),
            ("'(,5]'::int8range", "08000000080000000000000006", Range(None, 6, "[)")),
            ("'{}'::int4[]", "000000000000000000000017", []),
            ("'{}'::text[]", "000000000000000000000019", []),
            (
                "'{[1,3),[5,7)}'::int4multirange",
                "00000002000000110200000004000000010000000400000003"
                "000000110200000004000000050000000400000007",
                Multirange([Range(1, 3, "[)"), Range(5, 7, "[)")]),
            ),
            ("'{}'::int4multirange", "00000000", Multirange([])),
            (
                "array['{\"a\":1}'::jsonb, null]",
                "000000010000000100000eda000000020000000100000009017b2261223a20317dffffffff",
                [{"a": 1}, None],
            ),
            (
                "array['{\"a\":1}'::json]",
                "0000000100000000000000720000000100000001000000077b2261223a317d",
                [{"a": 1}],
            ),
            (
                "array['2020-01-01'::date, '2000-01-01'::date]",
                "00000001000000000000043a00000002000000010000000400001c890000000400000000",
                [dt.date(2020, 1, 1), dt.date(2000, 1, 1)],
            ),
        ]
        for expr, want_hex, want in cases:
            cur = conn.cursor(binary=True)
            cur.execute(f"select {expr}")
            assert cur.pgresult.fformat(0) == 1, expr
            assert cur.pgresult.get_value(0, 0).hex() == want_hex, expr
            if want is not None:
                assert cur.fetchone()[0] == want, expr

        # The same types read back from a TABLE (the faker's real shape), all
        # columns binary in one row -- the shape psycopg cannot mix formats in.
        cur = conn.cursor()
        cur.execute(
            "create table fk (d date, t time, tz timetz, ts timestamp, tstz timestamptz, "
            "iv interval, r int4range, mr int4multirange, j json[], jb jsonb[], e int4[])"
        )
        cur.execute(
            "insert into fk values ('2020-02-03', '01:02:03.5', '01:02:03+02', "
            "'2020-02-03 04:05:06.789', '2020-02-03 04:05:06+00', '3 mons 2 days 1 hour', "
            "'[1,10)', '{[1,2),[5,9)}', array['[1, 2]'::json], array['{\"b\": true}'::jsonb], '{}')"
        )
        cur = conn.cursor(binary=True)
        cur.execute("select * from fk")
        assert all(cur.pgresult.fformat(i) == 1 for i in range(11))
        assert cur.fetchone() == (
            dt.date(2020, 2, 3),
            dt.time(1, 2, 3, 500000),
            dt.time(1, 2, 3, tzinfo=dt.timezone(dt.timedelta(hours=2))),
            dt.datetime(2020, 2, 3, 4, 5, 6, 789000),
            dt.datetime(2020, 2, 3, 4, 5, 6, tzinfo=dt.timezone.utc),
            dt.timedelta(days=92, hours=1),
            Range(1, 10, "[)"),
            Multirange([Range(1, 2, "[)"), Range(5, 9, "[)")]),
            [[1, 2]],
            [{"b": True}],
            [],
        )


def test_tstz_ranges_and_arrays_render_in_the_session_zone(home: Path) -> None:
    """A `tstzrange` / `tstzmultirange` / `timestamptz[]` column's TEXT form
    renders each bound in the session zone with its offset, as PostgreSQL
    16 does: `["2020-01-01 01:00:00+01","2020-06-01 12:00:00+02")` under
    Europe/Rome. The bounds are stored as naive UTC and used to go out that
    way, so a client under any zone but UTC read the wrong instants.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        conn.execute(
            "create table tzr (r tstzrange, m tstzmultirange, a timestamptz[]); "
            "insert into tzr values ('[2020-01-01 00:00+00,2020-06-01 10:00+00)', "
            "'{[2020-01-01 00:00+00,2020-01-02 00:00+00)}', "
            "array['2020-01-01 00:00+00'::timestamptz])"
        )
        rome = dt.datetime(2020, 1, 1, 1, tzinfo=dt.timezone(dt.timedelta(hours=1)))
        for zone, raw in (
            (
                "UTC",
                (
                    b'["2020-01-01 00:00:00+00","2020-06-01 10:00:00+00")',
                    b'{["2020-01-01 00:00:00+00","2020-01-02 00:00:00+00")}',
                    b'{"2020-01-01 00:00:00+00"}',
                ),
            ),
            (
                "Europe/Rome",
                (
                    b'["2020-01-01 01:00:00+01","2020-06-01 12:00:00+02")',
                    b'{["2020-01-01 01:00:00+01","2020-01-02 01:00:00+01")}',
                    b'{"2020-01-01 01:00:00+01"}',
                ),
            ),
        ):
            conn.execute(f"set timezone to '{zone}'")
            cur = conn.cursor()
            cur.execute("select r, m, a from tzr")
            assert tuple(cur.pgresult.get_value(0, i) for i in range(3)) == raw, zone
            row = cur.fetchone()
            assert row[0].lower == rome and row[2] == [rome], zone
            # And the binary form is the same UTC instant whatever the zone.
            cur = conn.cursor(binary=True)
            cur.execute("select r, m, a from tzr")
            assert cur.pgresult.get_value(0, 0).hex() == (
                "020000000800023e0786c260000000000800024a01a067c800"
            )
            assert cur.pgresult.get_value(0, 2).hex() == (
                "0000000100000000000004a000000001000000010000000800023e0786c26000"
            )


def test_set_config_reports_the_time_zone_like_set_does(home: Path) -> None:
    """`set_config('TimeZone', ...)` sends the same ParameterStatus as `SET
    TimeZone` (PostgreSQL 16). psycopg builds a timestamptz's tzinfo from that
    report, so without it a zone change through `set_config` was invisible to
    the client's loader and every timestamptz came back in the old zone."""
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        assert conn.info.parameter_status("TimeZone") == "UTC"
        conn.execute("select set_config('TimeZone', 'Europe/Rome', false)")
        assert conn.info.parameter_status("TimeZone") == "Europe/Rome"
        got = conn.execute("select '2020-07-01 12:00+00'::timestamptz").fetchone()[0]
        assert got.utcoffset() == dt.timedelta(hours=2)


def test_jsonb_unicode_escapes_match_postgres(home: Path) -> None:
    """PostgreSQL 16's `jsonb` input: a surrogate PAIR becomes the character,
    a lone surrogate is `22P02 invalid input syntax for type json` (no value
    suffix), and `\\u0000` is `22P05 unsupported Unicode escape sequence`.
    `json` keeps every escape verbatim and only refuses `\\u0000` through an
    operator. The faker emits such strings, and the old parser combined a
    pair into two U+FFFD replacement characters.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("""select '"\\ud83d\\ude00"'::jsonb::text, '"\\u00e9"'::jsonb::text""")
        assert cur.fetchone() == ('"\U0001f600"', '"\u00e9"')
        for lone in ('"\\ud83d"', '"\\ude00"', '"\\ud83dx"', '"\\ud83d\\ud83d"', '{"a":"\\ud83d"}'):
            with pytest.raises(psycopg.errors.InvalidTextRepresentation) as exc:
                cur.execute(f"select '{lone}'::jsonb")
            assert str(exc.value).startswith("invalid input syntax for type json"), lone
            assert exc.value.diag.sqlstate == "22P02"
        with pytest.raises(psycopg.errors.UntranslatableCharacter) as exc:
            cur.execute("""select '"\\u0000"'::jsonb""")
        assert exc.value.diag.sqlstate == "22P05"
        assert str(exc.value).startswith("unsupported Unicode escape sequence")
        # `json` is verbatim, escapes and all.
        cur.execute("""select '"\\ud83d"'::json::text, '"\\u0000"'::json::text""")
        assert cur.fetchone() == ('"\\ud83d"', '"\\u0000"')
        with pytest.raises(psycopg.errors.InvalidTextRepresentation):
            cur.execute("""select '"\\ud83d"'::json ->> 0""")
        with pytest.raises(psycopg.errors.UntranslatableCharacter):
            cur.execute("""select '"\\u0000"'::json ->> 0""")


def test_copy_inside_a_transaction_stays_in_it(home: Path) -> None:
    """A simple-protocol COPY inside a transaction leaves the connection
    INTRANS, and a COPY of NO rows too (PostgreSQL 16: status 2, rowcount 0).

    The vendored pgwire answered CopyDone with `ReadyForQuery(Idle)` whatever
    the transaction state; psycopg then believed the connection idle, its
    `rollback()` sent nothing, and the COPY'd rows survived the rollback.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        conn.execute("create table cp (id int, s text)")
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        cur = conn.cursor()
        with cur.copy("copy cp from stdin") as cp:
            pass
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INTRANS
        assert cur.rowcount == 0
        with cur.copy("copy cp from stdin") as cp:
            cp.write("1\ta\n2\tb\n")
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INTRANS
        assert cur.rowcount == 2
        assert conn.execute("select count(*) from cp").fetchone() == (2,)
        conn.rollback()
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.IDLE
        assert conn.execute("select count(*) from cp").fetchone() == (0,)
        conn.rollback()
        # A COPY that fails leaves the transaction in error, as any statement.
        with (
            pytest.raises(psycopg.errors.InvalidTextRepresentation),
            cur.copy("copy cp from stdin") as cp,
        ):
            cp.write("x\ta\n")
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INERROR
        conn.rollback()


def test_copy_fills_the_columns_it_omits_from_their_defaults(home: Path) -> None:
    """A COPY with a column list fills the omitted columns like an INSERT
    does -- a `serial` from its sequence (PostgreSQL 16: `(1, None, 'hello'),
    (2, None, 'world')`), a literal default from its expression. The rows
    used to be stored with those columns NULL. And the fill runs INSIDE the
    open transaction: done outside, its `nextval` conflicted with the
    transaction's own earlier INSERT and spun on the write-conflict retry
    until the client gave up.
    """
    with _Server(home) as server, server.connect(autocommit=True) as conn:
        conn.execute(
            "create table ci (id serial primary key, n int, data text, k text default 'dflt')"
        )
        with conn.cursor().copy("copy ci (n, data) from stdin") as cp:
            cp.write("\\N\thello\n\\N\tworld\n")
        assert conn.execute("select * from ci order by id").fetchall() == [
            (1, None, "hello", "dflt"),
            (2, None, "world", "dflt"),
        ]
        conn.execute("create table ct (id serial primary key, n int, data text)")
    with _Server(home) as server, server.connect(autocommit=False) as conn:
        conn.execute("insert into ct (data) values ('a')")
        with conn.cursor().copy("copy ct (data) from stdin") as cp:
            cp.write("hello\n")
        conn.execute("insert into ct (data) values ('b')")
        assert conn.execute("select * from ct order by id").fetchall() == [
            (1, None, "a"),
            (2, None, "hello"),
            (3, None, "b"),
        ]
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INTRANS
        conn.rollback()
        assert conn.execute("select count(*) from ct").fetchone() == (0,)


def test_copy_in_parses_arrays_and_binary_reads_stored_timestamps(home: Path) -> None:
    """A COPY'd `text[]` is stored as the array it is (it was one raw string,
    and read back as `{"{ab,cd}"}`, which psycopg reports as "malformed
    array: hit the end of the buffer"), a COPY'd date in its canonical text;
    and a stored `timestamp` read by a BINARY cursor is the i64 instant, not
    its text bytes (psycopg read `2020-01-01 00:00:00` as an integer:
    "timestamp too large").
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table ca (id int, a text[], d date, ts timestamp, tz timestamptz)")
        with conn.cursor().copy("copy ca from stdin") as cp:
            cp.write("1\t{ab,cd}\t2020-01-05\t2020-01-01 00:00:00.25\t2020-01-01 00:00:00+00\n")
        conn.execute("set timezone to 'UTC'")
        for binary in (False, True):
            cur = conn.cursor(binary=binary)
            cur.execute("select a, d, ts, tz from ca")
            assert cur.pgresult.fformat(2) == int(binary)
            assert cur.fetchone() == (
                ["ab", "cd"],
                dt.date(2020, 1, 5),
                dt.datetime(2020, 1, 1, 0, 0, 0, 250000),
                dt.datetime(2020, 1, 1, tzinfo=dt.timezone.utc),
            )
        with conn.cursor().copy("copy ca (a) to stdout") as cp:
            assert b"".join(cp) == b"{ab,cd}\n"


def test_an_empty_multirange_binary_parameter_is_typed_from_its_column(home: Path) -> None:
    """psycopg sends `Multirange([])` UNTYPED (no element to name the type
    from) and, in binary, as four zero bytes. Read as text those were
    `malformed multirange literal: "\\0\\0\\0\\0"` -- and that message carried
    the NULs onto the wire, where the client found bytes after the message's
    fields and dropped the connection. PostgreSQL resolves the parameter's
    type from the column it goes into, and so does this server now.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("create table mr (id int, m int4multirange)")
        cur = conn.cursor()
        cur.execute("insert into mr values (%s, %b)", (1, Multirange([])))
        cur.execute("insert into mr values (%s, %b)", (2, Multirange([Range(1, 3, "[)")])))
        assert conn.execute("select m from mr order by id").fetchall() == [
            (Multirange([]),),
            (Multirange([Range(1, 3, "[)")]),),
        ]
        # The connection survived every step.
        assert conn.execute("select 1").fetchone() == (1,)


def test_datetime_array_text_is_not_a_debug_dump(home: Path) -> None:
    """`timestamp[]::text` / `interval[]::text` render each element as its
    scalar text (PostgreSQL 16: `{"2020-01-01 00:00:00.5"}`, `{"1 day"}`).
    They rendered the BSON value's Rust Debug form, `{"DateTime(2020-01-01
    0:00:00.5 +00:00:00)"}`, which no client can read.
    """
    with _Server(home) as server, server.connect() as conn:
        conn.execute("set timezone to 'UTC'")
        cur = conn.cursor()
        cur.execute(
            "select array['2020-01-01 00:00:00.5'::timestamp]::text, "
            "array['1 day'::interval]::text, array['12:00'::time]::text"
        )
        assert cur.fetchone() == ('{"2020-01-01 00:00:00.5"}', '{"1 day"}', "{12:00:00}")
