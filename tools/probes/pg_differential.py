"""Run a corpus of SQL against SecantusDB's PG server AND a real PostgreSQL,
and print only the disagreements.

The SQL twin of the mongod probes beside it. It drives **both servers through
the same ``psycopg`` connection class**, so client-side type mapping is
identical on both sides and every difference that shows up is the server's.

**Do not probe through ``run_sql()`` instead.** The embedded API returns the
engine's internal values, which the wire layer converts on the way out -- a
``numeric(p,s)`` cast surfaces as a raw ``Decimal128``, a ``::date`` as a
``str``, an interval as a subdocument. Probed directly, all three look like
divergences; over the wire all three are correct. That was 4 of the first 8
"findings" in the 2026-09-03 sweep, chased before the harness moved to the wire.

**Capture the column TYPES too** (``--types``). A wrong oid under a right value
is invisible to a row comparison, and several real bugs were exactly that:
``#>`` sent jsonb under oid 25, ``LIKE ... ESCAPE`` sent a boolean as ``'t'``
under oid 25, and a window ``sum(int4)`` declared int4 where PG promotes to
int8.

Usage::

    uv run --no-sync python tools/probes/pg_differential.py \\
        tools/probes/pg_corpora/windows.setup.sql \\
        tools/probes/pg_corpora/windows.sql --types

A corpus is two files: a ``.setup.sql`` run on both servers first (its failures
are reported but do not stop the run) and the corpus proper, one statement per
line, ``#`` for a comment. Statements run IN ORDER against both servers, so a
corpus may build on its own writes.

``SECANTUS_PG_ORACLE_DSN`` overrides the reference server (default: the local
PostgreSQL that ``tests/pg_oracle.py`` uses).

**Check ``SHOW lc_ctype`` before believing a case-mapping difference.** This
box's PostgreSQL runs the ``C`` locale, which does not case-map non-ASCII at all
-- ``upper('é')`` is ``é`` there. Several apparent ``initcap`` / ``upper`` bugs
are that, not the engine.
"""

from __future__ import annotations

import datetime
import decimal
import os
import shutil
import sys
import tempfile

import psycopg

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "src"))

from secantus.sql.pgserver import SecantusPGServer  # noqa: E402
from secantus.storage import Storage  # noqa: E402

DEFAULT_DSN = "host=127.0.0.1 port=5432 dbname=postgres user=jdrumgoole"


def _norm(v: object) -> tuple:
    """A value in a form that compares across the two servers' drivers.

    Numbers are compared by VALUE, not by Python type or trailing zeros -- the
    two sides legitimately hand back int vs Decimal for the same column.
    """
    if isinstance(v, bool):
        return ("bool", v)
    if isinstance(v, decimal.Decimal):
        return ("num", "NaN" if v.is_nan() else format(v.normalize(), "f"))
    if isinstance(v, float):
        return ("num", format(decimal.Decimal(repr(v)).normalize(), "f"))
    if isinstance(v, int):
        return ("num", str(v))
    if isinstance(v, datetime.datetime):
        return ("ts", v.isoformat())
    if isinstance(v, datetime.date):
        return ("date", v.isoformat())
    if isinstance(v, datetime.time):
        return ("time", v.isoformat())
    if isinstance(v, (memoryview, bytes)):
        return ("bytes", bytes(v).hex())
    if isinstance(v, (list, tuple)):
        return ("list", tuple(_norm(x) for x in v))
    if isinstance(v, str):
        return ("str", v)
    return (type(v).__name__, repr(v))


def _run(cur: object, sql: str, want_types: bool) -> tuple:
    try:
        cur.execute(sql)
        if cur.description is None:
            return ("ok", [], cur.statusmessage, [])
        rows = [tuple(_norm(c) for c in r) for r in cur.fetchall()]
        types = [d.type_code for d in cur.description] if want_types else []
        return ("ok", rows, cur.statusmessage, types)
    except Exception as exc:  # noqa: BLE001 — the error IS the observation
        code = getattr(getattr(exc, "diag", None), "sqlstate", None)
        return ("err", f"{code}: {str(exc).splitlines()[0]}", None, [])


def _read(path: str) -> list[str]:
    with open(path) as fh:
        return [ln.rstrip() for ln in fh if ln.strip() and not ln.lstrip().startswith("#")]


def main(setup_path: str, corpus_path: str, *, types: bool, tags: bool) -> int:
    store_dir = tempfile.mkdtemp(prefix="pgprobe-")
    store = Storage(store_dir)
    srv = SecantusPGServer(port=0, storage=store)
    srv.start()
    host, port = srv.address
    sec = psycopg.connect(host=host, port=port, dbname="db", user="probe", autocommit=True)
    ref = psycopg.connect(os.environ.get("SECANTUS_PG_ORACLE_DSN", DEFAULT_DSN), autocommit=True)
    scur, rcur = sec.cursor(), ref.cursor()

    for stmt in _read(setup_path):
        _run(rcur, stmt, False)
        got = _run(scur, stmt, False)
        if got[0] == "err" and not stmt.upper().startswith("DROP"):
            print(f"SETUP FAILED (secantus): {stmt}\n  -> {got[1]}")

    corpus = _read(corpus_path)
    diffs = 0
    for sql in corpus:
        want = _run(rcur, sql, types)
        got = _run(scur, sql, types)
        rowdiff = (want[0], want[1]) != (got[0], got[1])
        both_ok = want[0] == got[0] == "ok"
        tagdiff = tags and both_ok and want[2] != got[2]
        typediff = types and both_ok and want[3] != got[3]
        if not (rowdiff or tagdiff or typediff):
            continue
        kind = (
            "ERRDIFF"
            if want[0] == got[0] == "err"
            else "SHOULD-ERROR"
            if want[0] == "err"
            else "SPURIOUS-ERROR"
            if got[0] == "err"
            else "VALUE"
            if rowdiff
            else "TYPE"
            if typediff
            else "TAG"
        )
        diffs += 1
        print(f"--- {kind}\n  SQL: {sql}\n   PG: {want[1]}\n  SEC: {got[1]}")
        if typediff:
            print(f"  PGTYPES: {want[3]}\n SECTYPES: {got[3]}")
        if tagdiff:
            print(f"  PGTAG: {want[2]}\n SECTAG: {got[2]}")

    print(f"\n=== {diffs} divergences out of {len(corpus)}")
    sec.close()
    ref.close()
    srv.stop()
    store.close()
    shutil.rmtree(store_dir, ignore_errors=True)
    return 1 if diffs else 0


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 2:
        print(__doc__)
        sys.exit(2)
    sys.exit(
        main(args[0], args[1], types="--types" in sys.argv, tags="--tag" in sys.argv)
    )
