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

**Parameters exercise a different server path than literals do.** A corpus line
may carry them after a ``ALSO_SEP`` separator, as a Python list literal::

    SELECT %s::int + 1 ||| [5]
    SELECT %s ||| [datetime.date(2020, 1, 5)]

Such a line is run THREE ways -- unnamed-portal text params, a server-side
PREPARED statement (psycopg's ``prepare=True``, which is what an ORM's
statement cache produces), and BINARY result format -- and each is compared
separately, so the report names the binding mode that diverged. This matters
because the extended protocol is what every real driver actually speaks:
psycopg, JDBC and most ORMs send Bind messages with typed parameters rather
than the interpolated SQL text that a literal corpus produces. Two real bugs
here were binary-format-only.

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


#: Separates a corpus line's SQL from its Python-literal parameter list.
PARAM_SEP = "|||"

#: Names a corpus line's parameter literal may use.
_PARAM_NS = {
    "datetime": datetime,
    "decimal": decimal,
    "date": datetime.date,
    "time": datetime.time,
    "Decimal": decimal.Decimal,
    "None": None,
    "True": True,
    "False": False,
}


def _run_params(
    cur: object, sql: str, args: list, want_types: bool, *, prepare: bool, binary: bool
) -> tuple:
    """One parameterised execution. `prepare` forces a server-side prepared
    statement (an ORM's statement cache); `binary` asks for binary results."""
    try:
        cur.execute(sql, args, prepare=prepare, binary=binary)
        if cur.description is None:
            return ("ok", [], cur.statusmessage, [])
        rows = [tuple(_norm(c) for c in r) for r in cur.fetchall()]
        types = [d.type_code for d in cur.description] if want_types else []
        return ("ok", rows, cur.statusmessage, types)
    except Exception as exc:  # noqa: BLE001 — the error IS the observation
        code = getattr(getattr(exc, "diag", None), "sqlstate", None)
        return ("err", f"{code}: {str(exc).splitlines()[0]}", None, [])


#: (label, prepare, binary) for each binding path a corpus line is run through.
PARAM_MODES = (("text", False, False), ("prepared", True, False), ("binary", False, True))


def _split_params(line: str) -> tuple[str, list | None]:
    """`("SELECT %s", [5])` for a parameterised corpus line, `(line, None)` for
    a plain one. A bad literal is reported rather than silently skipped."""
    if PARAM_SEP not in line:
        return line, None
    sql, _, raw = line.partition(PARAM_SEP)
    try:
        args = eval(raw.strip(), {"__builtins__": {}}, _PARAM_NS)  # noqa: S307 — dev probe
    except Exception as exc:  # noqa: BLE001
        print(f"BAD PARAMS: {line}\n  -> {exc}")
        return sql.strip(), None
    return sql.strip(), list(args)


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
    checks = 0
    for line in corpus:
        sql, args = _split_params(line)
        # A parameterised line is compared once per BINDING MODE, so the report
        # names which one diverged; a plain line keeps the single comparison.
        runs = (
            [
                (
                    label,
                    _run_params(rcur, sql, args, types, prepare=p, binary=b),
                    _run_params(scur, sql, args, types, prepare=p, binary=b),
                )
                for label, p, b in PARAM_MODES
            ]
            if args is not None
            else [("", _run(rcur, sql, types), _run(scur, sql, types))]
        )
        for label, want, got in runs:
            checks += 1
            diffs += _report(sql, label, want, got, types=types, tags=tags)

    print(f"\n=== {diffs} divergences out of {checks} checks ({len(corpus)} corpus lines)")
    sec.close()
    ref.close()
    srv.stop()
    store.close()
    shutil.rmtree(store_dir, ignore_errors=True)
    return 1 if diffs else 0


def _report(sql: str, label: str, want: tuple, got: tuple, *, types: bool, tags: bool) -> int:
    """Print one comparison if it diverged; return 1 when it did."""
    rowdiff = (want[0], want[1]) != (got[0], got[1])
    both_ok = want[0] == got[0] == "ok"
    tagdiff = tags and both_ok and want[2] != got[2]
    typediff = types and both_ok and want[3] != got[3]
    if not (rowdiff or tagdiff or typediff):
        return 0
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
    where = f" [{label}]" if label else ""
    print(f"--- {kind}{where}\n  SQL: {sql}\n   PG: {want[1]}\n  SEC: {got[1]}")
    if typediff:
        print(f"  PGTYPES: {want[3]}\n SECTYPES: {got[3]}")
    if tagdiff:
        print(f"  PGTAG: {want[2]}\n SECTAG: {got[2]}")
    return 1


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 2:
        print(__doc__)
        sys.exit(2)
    sys.exit(main(args[0], args[1], types="--types" in sys.argv, tags="--tag" in sys.argv))
