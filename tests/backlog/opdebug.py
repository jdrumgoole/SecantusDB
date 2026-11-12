"""Reading an operation's `Slow query` lines and profile entries from a
server, for comparing them with mongod's (RM-023 and the items it left).

`logged(client, is_mongod, setup, op)` runs `setup(db)` on a fresh database,
then `op(db)` with every operation logged as slow and profiled, and returns
the attributes of the `Slow query` lines `op` wrote (in order, with each
line's component) and its database's profile entries. mongod logs every
operation at `slowms: -1`; the Rust server refuses a negative `slowms` and
logs every operation at 0.
"""

from __future__ import annotations

import contextlib
import json
import uuid
from collections.abc import Callable, Iterator
from typing import Any

from bson import json_util

#: Commands a window of the log holds that the operation did not run: the
#: profiler's own, the read of the log, the driver's monitoring.
_NOT_THE_OPERATION = {
    "profile",
    "getLog",
    "hello",
    "isMaster",
    "ismaster",
    "endSessions",
    # An index build of the setup can log on its own thread after the
    # command returned (seen in CI under load, 2026-10-25).
    "createIndexes",
}


def fresh(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex[:8]}"


def seed(db: Any) -> None:
    """Twelve documents over an index on `x`."""
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
    db.c.create_index("x")


def _first(command: Any) -> str:
    return next(iter(command), "") if isinstance(command, dict) else str(command)


def logged(
    client: Any,
    is_mongod: bool,
    setup: Callable[[Any], Any],
    op: Callable[[Any], Any],
    prefix: str = "opdebug",
    name: str | None = None,
    keep: frozenset[str] = frozenset(),
) -> tuple[list[tuple[str, dict]], list[dict]]:
    """`op`'s `Slow query` lines, as `(component, attr)`, and its database's
    profile entries. `name` names the database, for a comparison of what
    depends on it (a query shape's hash covers its namespace). `keep`: the
    commands otherwise left out as not the operation's that `op` runs (a
    `createIndexes` whose line is compared), whose setup must not run them."""
    dropped = _NOT_THE_OPERATION - keep
    db = client[name or fresh(prefix)]
    setup(db)
    log = client.admin.command("getLog", "global")["log"]
    last = log[-1] if log else None
    db.command("profile", 2, slowms=-1 if is_mongod else 0)
    try:
        # A failing operation is logged too.
        with contextlib.suppress(Exception):
            op(db)
    finally:
        db.command("profile", 0, slowms=100)
    log = client.admin.command("getLog", "global")["log"]
    if last is not None:
        for i in range(len(log) - 1, -1, -1):
            if log[i] == last:
                log = log[i + 1 :]
                break
    lines = []
    for raw in log:
        entry = json.loads(raw)
        if entry.get("msg") != "Slow query":
            continue
        attr = entry["attr"]
        if not str(attr.get("ns", "")).startswith(db.name + "."):
            continue
        if _first(attr.get("command", {})) in dropped:
            continue
        lines.append((entry["c"], attr))
    entries = [
        json.loads(json_util.dumps(e))
        for e in db.system.profile.find().sort("$natural", 1)
        if _first(e.get("command", {})) not in _NOT_THE_OPERATION
    ]
    client.drop_database(db.name)
    return lines, entries


@contextlib.contextmanager
def no_timed_yields(mongod: Any) -> Iterator[None]:
    """mongod with its executor's time-based yields off, as RM-185's
    regression tests run it. mongod also yields -- saves every stage, and
    releases and re-takes its locks and flow control ticket -- once an
    operation has run `internalQueryExecYieldPeriodMS` (10 ms), which load
    decides: a `delete_many` of six documents yielded once in 60 runs under
    six concurrent writers (`numYields: 1`, a third write lock round,
    `flowControl: {acquireCount: 2}`) and in none of 120 with the period off
    (2026-11-07). Run without it, the counts are the operation's own.
    This server's plan executors keep the same period since RM-360, so a
    test of what a yield changes turns it off on both servers."""
    was = mongod.admin.command("setParameter", 1, internalQueryExecYieldPeriodMS=86_400_000)
    try:
        yield
    finally:
        mongod.admin.command("setParameter", 1, internalQueryExecYieldPeriodMS=was["was"])


def keys(doc: dict, leave_out: frozenset[str] = frozenset()) -> list[str]:
    return [k for k in doc if k not in leave_out]
