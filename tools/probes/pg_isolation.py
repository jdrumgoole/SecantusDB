"""PostgreSQL's isolation tests (`src/test/isolation/specs/*.spec`) against
two servers, step by step.

A small stand-in for PostgreSQL's `isolationtester`: it parses a spec (its
setup and teardown, its sessions and their steps, its permutations -- or,
with none, every interleaving of the sessions' steps), runs each
permutation with one connection per session, and records each step's
outcome: its rows or its error, or that it is still running after `--wait`
seconds (blocked), and when a blocked step completes. Each step is sent as
one simple-protocol query (`PQsendQuery`), as `isolationtester` sends it.

Blocking is judged by time, the same way for both servers, where
`isolationtester` asks `pg_isolation_test_session_is_blocked`.

    python tools/probes/pg_isolation.py SPEC... \\
        --a "host=127.0.0.1 port=5499 user=postgres" \\
        --b "host=127.0.0.1 port=27419 user=postgres"

prints each permutation whose outcomes differ, and a summary. With
`--record DIR` it runs only `--a` and writes each spec's traces to
`DIR/<spec>.json`, the expected outcomes `tests/backlog/test_rp_isolation.py`
replays.
"""

from __future__ import annotations

import argparse
import itertools
import json
import re
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import psycopg
from psycopg import pq


@dataclass
class Session:
    name: str
    setup: list[str] = field(default_factory=list)
    teardown: list[str] = field(default_factory=list)
    steps: list[tuple[str, str]] = field(default_factory=list)


@dataclass
class Spec:
    setup: list[str] = field(default_factory=list)
    teardown: list[str] = field(default_factory=list)
    sessions: list[Session] = field(default_factory=list)
    permutations: list[list[str]] = field(default_factory=list)


_TOKEN = re.compile(r'\s+|#[^\n]*|\{|"[^"]*"|\(|\)|\*|[A-Za-z_0-9]+')


def parse_spec(text: str) -> Spec:
    """`specparse.y` and `specscanner.l`: an SQL block runs from `{` to the
    first `}`."""
    tokens: list[tuple[str, str]] = []
    i = 0
    while i < len(text):
        if text[i] == "{":
            end = text.index("}", i)
            tokens.append(("sql", text[i + 1 : end].strip()))
            i = end + 1
            continue
        m = _TOKEN.match(text, i)
        if not m:
            raise ValueError(f"cannot read the spec at {text[i : i + 20]!r}")
        t = m.group(0)
        i = m.end()
        if t.isspace() or t.startswith("#"):
            continue
        if t.startswith('"'):
            tokens.append(("id", t[1:-1]))
        elif t in "()*":
            tokens.append((t, t))
        else:
            tokens.append(("id", t))
    spec = Spec()
    k = 0
    session: Session | None = None
    while k < len(tokens):
        kind, val = tokens[k]
        if kind == "id" and val == "setup":
            (session.setup if session else spec.setup).append(tokens[k + 1][1])
            k += 2
        elif kind == "id" and val == "teardown":
            (session.teardown if session else spec.teardown).append(tokens[k + 1][1])
            k += 2
        elif kind == "id" and val == "session":
            session = Session(tokens[k + 1][1])
            spec.sessions.append(session)
            k += 2
        elif kind == "id" and val == "step":
            assert session is not None
            session.steps.append((tokens[k + 1][1], tokens[k + 2][1]))
            k += 3
        elif kind == "id" and val == "permutation":
            session = None
            k += 1
            perm: list[str] = []
            while k < len(tokens) and tokens[k][0] in ("id", "("):
                if tokens[k][0] == "(":
                    # A blocker annotation: the time-based wait stands in.
                    while tokens[k][0] != ")":
                        k += 1
                    k += 1
                    continue
                if tokens[k][1] in ("permutation",):
                    break
                perm.append(tokens[k][1])
                k += 1
            spec.permutations.append(perm)
        else:
            raise ValueError(f"unexpected {val!r} in the spec")
    if not spec.permutations:
        spec.permutations = list(_interleavings([[s for s, _ in x.steps] for x in spec.sessions]))
    return spec


def _interleavings(lists: list[list[str]]):
    """Every merge of the lists keeping each list's order (`run_all_permutations`)."""
    if all(not lst for lst in lists):
        yield []
        return
    for i, lst in enumerate(lists):
        if lst:
            rest = [x if j != i else x[1:] for j, x in enumerate(lists)]
            for tail in _interleavings(rest):
                yield [lst[0], *tail]


def _results(conn: psycopg.Connection) -> list[Any]:
    """Every result of the query just sent: rows, a command tag, or an error."""
    out: list[Any] = []
    pg = conn.pgconn
    while True:
        r = pg.get_result()
        if r is None:
            return out
        if r.status == pq.ExecStatus.TUPLES_OK:
            rows = [
                tuple(
                    None if r.get_value(i, j) is None else r.get_value(i, j).decode()
                    for j in range(r.nfields)
                )
                for i in range(r.ntuples)
            ]
            out.append(("rows", rows))
        elif r.status == pq.ExecStatus.FATAL_ERROR:
            state = (r.error_field(pq.DiagnosticField.SQLSTATE) or b"").decode()
            msg = (r.error_field(pq.DiagnosticField.MESSAGE_PRIMARY) or b"").decode()
            detail = (r.error_field(pq.DiagnosticField.MESSAGE_DETAIL) or b"").decode()
            # An xid in a reason code is the cluster's own count.
            detail = re.sub(r"(pivot|transaction) \d+", r"\1 <xid>", detail)
            out.append(("ERROR", state, msg, detail))
        else:
            out.append(("ok", (r.command_status or b"").decode()))


def _ready(conn: psycopg.Connection, wait: float) -> bool:
    pg = conn.pgconn
    deadline = time.monotonic() + wait
    while True:
        pg.consume_input()
        if not pg.is_busy():
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.005)


def _run_sql(conn: psycopg.Connection, sql: str, wait: float = 600.0) -> list[Any]:
    conn.pgconn.send_query(sql.encode())
    if not _ready(conn, wait):
        return [("timeout",)]
    return _results(conn)


def run_permutation(spec: Spec, perm: list[str], dsn: str, wait: float) -> list[Any]:
    owner = {step: s for s in spec.sessions for step, _ in s.steps}
    sql_of = {step: sql for s in spec.sessions for step, sql in s.steps}
    trace: list[Any] = []
    ctl = psycopg.connect(dsn, autocommit=True)
    conns: dict[str, psycopg.Connection] = {}
    try:
        for block in spec.setup:
            trace.append(("setup", _run_sql(ctl, block)))
        for s in spec.sessions:
            conns[s.name] = psycopg.connect(dsn, autocommit=True)
            for block in s.setup:
                trace.append((f"{s.name} setup", _run_sql(conns[s.name], block)))
        waiting: list[str] = []
        for step in perm:
            c = conns[owner[step].name]
            c.pgconn.send_query(sql_of[step].encode())
            if _ready(c, wait):
                trace.append((step, _results(c)))
            else:
                trace.append((step, "waiting"))
                waiting.append(step)
            # A step this one unblocked completes now (`isolationtester`
            # checks the waiting steps after each step).
            for w in list(waiting):
                if w == step:
                    continue
                wc = conns[owner[w].name]
                if _ready(wc, min(wait, 0.5)):
                    trace.append((f"{w} completed", _results(wc)))
                    waiting.remove(w)
        for w in waiting:
            wc = conns[owner[w].name]
            if _ready(wc, wait * 4):
                trace.append((f"{w} completed", _results(wc)))
            else:
                trace.append((f"{w} still waiting",))
                wc.pgconn.request_cancel()
                _ready(wc, 10)
                _results(wc)
        for s in spec.sessions:
            for block in s.teardown:
                trace.append((f"{s.name} teardown", _run_sql(conns[s.name], block)))
    finally:
        for c in conns.values():
            c.close()
        for block in spec.teardown:
            _run_sql(ctl, block)
        ctl.close()
    return trace


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("specs", nargs="+")
    ap.add_argument("--a", required=True, help="the reference server's DSN")
    ap.add_argument("--b", help="the server under test's DSN")
    ap.add_argument("--record", help="write --a's traces to this directory")
    ap.add_argument("--wait", type=float, default=2.0)
    ap.add_argument("--show", action="store_true", help="print every trace")
    args = ap.parse_args()
    if args.record:
        for path in args.specs:
            spec = parse_spec(Path(path).read_text())
            traces = {
                " ".join(p): run_permutation(spec, p, args.a, args.wait) for p in spec.permutations
            }
            out = Path(args.record) / f"{Path(path).stem}.json"
            # One permutation a line.
            lines = [f"{json.dumps(k)}: {json.dumps(v)}" for k, v in traces.items()]
            out.write_text("{\n" + ",\n".join(lines) + "\n}\n")
            print(f"{out}: {len(traces)} permutations")
        return 0
    if not args.b:
        ap.error("--b is required without --record")
    total = differ = 0
    for path in args.specs:
        spec = parse_spec(Path(path).read_text())
        bad = 0
        for perm in spec.permutations:
            total += 1
            a = run_permutation(spec, perm, args.a, args.wait)
            b = run_permutation(spec, perm, args.b, args.wait)
            if a != b:
                differ += 1
                bad += 1
                print(f"--- {Path(path).stem}: {' '.join(perm)}")
                for x, y in itertools.zip_longest(a, b):
                    mark = "  " if x == y else "!!"
                    print(f"{mark} A {x}")
                    if x != y:
                        print(f"{mark} B {y}")
            elif args.show:
                print(f"=== {Path(path).stem}: {' '.join(perm)}")
                for x in a:
                    print(f"   {x}")
        print(
            f"{Path(path).stem}: {len(spec.permutations) - bad} of {len(spec.permutations)} agree"
        )
        sys.stdout.flush()
    print(f"TOTAL {total - differ} of {total} agree")
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main())
