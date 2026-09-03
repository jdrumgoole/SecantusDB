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

import datetime as dt
import shutil
import socket
import subprocess
import time
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
        self.proc = subprocess.Popen(
            [str(BINARY), str(self.home), f"127.0.0.1:{self.port}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                out = self.proc.stdout.read() if self.proc.stdout else ""
                raise RuntimeError(f"secantusd-pg exited: {out}")
            try:
                with socket.create_connection(("127.0.0.1", self.port), timeout=0.25):
                    return self
            except OSError:
                time.sleep(0.05)
        raise RuntimeError("secantusd-pg did not start")

    def __exit__(self, *exc: object) -> None:
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)

    def connect(self) -> psycopg.Connection:
        return psycopg.connect(
            f"host=127.0.0.1 port={self.port} dbname=postgres user=test",
            autocommit=True,
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

        # A nested array is refused, not flattened. rust-postgres encodes one
        # dimension only, and the flattening it produced turned `{{1,2},{3,4}}`
        # into two elements whose text was `{1,2}` and `{3,4}` — indistinguish-
        # able, at the client, from a real answer.
        with pytest.raises(psycopg.Error) as exc:
            cur.execute("SELECT '{{1,2},{3,4}}'::int[]")
        assert exc.value.diag.sqlstate == "0A000"


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

    `DEALLOCATE <name>` is still refused rather than treated as a no-op:
    PostgreSQL answers 26000 for a name that does not exist, and silently
    succeeding would be a wrong answer.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        cur.execute("DEALLOCATE ALL")
        assert cur.statusmessage == "DEALLOCATE ALL"

        with pytest.raises(psycopg.Error) as exc:
            cur.execute("DEALLOCATE nosuchstmt")
        assert exc.value.diag.sqlstate == "0A000"


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


def test_regtype_names_a_type(home: Path) -> None:
    """`'int4'::regtype` is the type it names, printed as PostgreSQL prints it."""
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for given, want in [("int4", "integer"), ("integer", "integer"), ("text", "text")]:
            cur.execute("select %s::regtype::text", (given,))
            assert cur.fetchone()[0] == want


def test_timestamptz_columns_are_refused_not_silently_wrong(home: Path) -> None:
    """A `timestamptz` COLUMN is refused, because storing one would be wrong.

    `timestamptz` is kept as canonical text here, the way `date` and `time`
    already are — but a timestamptz *renders in the session's zone*, so that
    text is only correct for the session that wrote it. Before this refusal, a
    row written under UTC read back as `12:00:00+00` under `Europe/Rome`, where
    PostgreSQL answers `13:00:00+01`: the right instant printed in the wrong
    zone, which no client could detect.

    The type still works everywhere it is a value rather than storage.
    """
    with _Server(home) as server, server.connect() as conn:
        cur = conn.cursor()
        for ddl in (
            "create table t (id int primary key, ts timestamptz)",
            "create table u (id int primary key, tt timetz)",
        ):
            with pytest.raises(psycopg.Error) as exc:
                cur.execute(ddl)
            assert exc.value.diag.sqlstate == "0A000"

        # A plain `timestamp` column is unaffected, and the tz types still work
        # as casts and bound values.
        cur.execute("create table v (id int primary key, ts timestamp)")
        cur.execute("select '2026-01-01 12:00'::timestamptz::text")
        assert cur.fetchone()[0] == "2026-01-01 12:00:00+00"


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
