"""``COPY … TO STDOUT`` is a cancellation point.

COPY OUT used to materialise the whole result and hand it to a single
``sendall``, so a client that read one row and abandoned the copy found it
already finished: the transaction stayed ``INTRANS`` where PostgreSQL leaves it
``INERROR``, and a following statement ran instead of getting ``25P02``. There
was no point at which the stream could observe the CancelRequest — even though
the machinery to deliver one already existed, and ``sql/session.py`` had
described "the COPY TO row stream" as a cancellation point since before it was
one.

**The row count matters.** With a small copy BOTH servers buffer the whole
result and end ``INTRANS``, so a 100-row reproducer shows nothing; the
divergence only appears once the server is genuinely still streaming. Probed
against PostgreSQL 14.13 at 200k rows.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

psycopg = pytest.importorskip("psycopg")

#: What decides whether the server is still streaming is BYTES on the socket,
#: not row count — so this uses few, WIDE rows. 2,000 x ~4 KB is ~8 MB, far
#: past any socket buffer, and seeds in a fraction of the time 200k narrow
#: rows took. (The first cut used 200k narrow rows and blew the 25-minute hang
#: watchdog on CI's durable lane, where every insert is fully journalled.)
_WIDE_ROWS = 2_000
_WIDTH = 4_000


@pytest.fixture()
def server(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        st.close()


def _connect(srv):
    host, port = srv.address
    return psycopg.connect(host=host, port=port, dbname="db", user="joe")


def _seed(cur, rows, width=1):
    """Seed with a single set-returning INSERT — `executemany` of this many
    rows is what made the durable CI lane time out."""
    cur.execute("CREATE TABLE cp (id int, pad text)")
    cur.execute(
        "INSERT INTO cp (id, pad) SELECT g, repeat('x', %s) FROM generate_series(1, %s) g",
        (width, rows),
    )


def test_abandoning_a_large_copy_out_aborts_the_block(server):
    with _connect(server) as conn:
        cur = conn.cursor()
        _seed(cur, _WIDE_ROWS, _WIDTH)
        conn.commit()
        with pytest.raises(ZeroDivisionError), cur.copy("COPY cp TO STDOUT") as copy:
            next(iter(copy))
            raise ZeroDivisionError("client gives up mid-copy")
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INERROR
        with pytest.raises(psycopg.errors.InFailedSqlTransaction):
            cur.execute("SELECT 1")
        conn.rollback()


def test_a_small_copy_out_still_completes(server):
    """The control. A copy that fits in the buffer finishes before the client
    can abandon it — PostgreSQL leaves that block usable, and so must we."""
    with _connect(server) as conn:
        cur = conn.cursor()
        _seed(cur, 100)
        conn.commit()
        with pytest.raises(ZeroDivisionError), cur.copy("COPY cp TO STDOUT") as copy:
            next(iter(copy))
            raise ZeroDivisionError("client gives up")
        assert conn.info.transaction_status == psycopg.pq.TransactionStatus.INTRANS
        cur.execute("SELECT 1")
        assert cur.fetchone() == (1,)
        conn.rollback()


#: (COPY suffix, extra blocks beyond one per row). The binary format's
#: trailing `int16 -1` is its own CopyData message, so iterating the raw copy
#: yields one block more than there are rows — PostgreSQL 14.13 does the same
#: (500 rows -> 501 blocks), so it is the expectation, not a defect.
_COPY_FORMATS = [("", 0), (" (FORMAT csv)", 0), (" (FORMAT binary)", 1)]


@pytest.mark.parametrize(("fmt", "extra"), _COPY_FORMATS)
def test_a_completed_copy_out_still_returns_every_row(server, fmt, extra):
    """Chunked flushing must not drop or duplicate rows in any format — the
    row count has to survive being split across several ``sendall`` calls."""
    rows = 2000
    with _connect(server) as conn:
        cur = conn.cursor()
        _seed(cur, rows)
        conn.commit()
        n = 0
        with cur.copy(f"COPY cp TO STDOUT{fmt}") as copy:
            for _ in copy:
                n += 1
        assert n == rows + extra
        conn.rollback()
