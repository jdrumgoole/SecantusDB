"""The awaitable `hello` (streaming SDAM) against mongod: argument parsing and timing.

A driver's server monitor sends `hello` with `topologyVersion` and
`maxAwaitTimeMS`, streamed with `exhaustAllowed` or not. mongod HOLDS the reply
for the whole budget when the client already has the current topology and
answers at once when it does not; the Go driver counts the messages and fails
`TestSDAMProse/heartbeats_processed_more_frequently` when a server answers more
often. Measured on 8.2.11 (2026-09-30), the Rust server:

* sent the FIRST streamed reply immediately -- 5 replies in 2s where mongod
  sends 4, which is the Go failure (12 monitor messages against a ceiling of 10);
* held a client naming ANOTHER process or an older counter for the full budget,
  where mongod answers it at once;
* accepted every malformed `topologyVersion` / `maxAwaitTimeMS` below.

Compares the Rust server only; the Python server is not the reference.

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27056/?directConnection=true" \\
        python tools/probes/awaitable_hello.py

Timings are bucketed (held about the budget vs answered at once), never
compared raw. An "older counter" needs a counter above 0, which a server that
has never changed topology does not have, so that case runs only where both
servers can express it.
"""

from __future__ import annotations

import os
import socket
import struct
import sys
import time
from typing import Any
from urllib.parse import urlsplit

import bson
import pymongo
from bson import Decimal128, Int64, ObjectId

MONGOD = os.environ.get("PROBE_MONGOD")
SERVER = os.environ.get("PROBE_SERVER")
BUDGET_MS = 500
EXHAUST_ALLOWED = 1 << 16
MORE_TO_COME = 1 << 1


def _address(uri: str) -> tuple[str, int]:
    host, _, port = urlsplit(uri).netloc.rpartition(":")
    return host, int(port)


class Wire:
    """A bare OP_MSG connection: pymongo will not send an exhaust `hello`."""

    def __init__(self, uri: str) -> None:
        self.sock = socket.create_connection(_address(uri), timeout=3)
        self.request_id = 0

    def send(self, doc: dict[str, Any], flags: int = 0) -> None:
        body = struct.pack("<I", flags) + b"\x00" + bson.encode(doc)
        self.request_id += 1
        self.sock.sendall(struct.pack("<iiii", 16 + len(body), self.request_id, 0, 2013) + body)

    def _read(self, n: int) -> bytes:
        data = b""
        while len(data) < n:
            chunk = self.sock.recv(n - len(data))
            if not chunk:
                raise ConnectionError("closed")
            data += chunk
        return data

    def recv(self) -> tuple[int, dict[str, Any]]:
        length = struct.unpack("<i", self._read(16)[:4])[0]
        body = self._read(length - 16)
        return struct.unpack("<I", body[:4])[0], bson.decode(body[5:])

    def close(self) -> None:
        self.sock.close()


def current_topology(uri: str) -> dict[str, Any]:
    w = Wire(uri)
    w.send({"hello": 1, "$db": "admin"})
    tv = dict(w.recv()[1]["topologyVersion"])
    w.close()
    return tv


def held(seconds: float) -> str:
    return "held" if seconds >= BUDGET_MS / 1000 * 0.8 else "immediate"


def one_reply(uri: str, tv: Any, exhaust: bool) -> tuple[Any, ...]:
    w = Wire(uri)
    start = time.monotonic()
    w.send(
        {"hello": 1, "topologyVersion": tv, "maxAwaitTimeMS": BUDGET_MS, "$db": "admin"},
        flags=EXHAUST_ALLOWED if exhaust else 0,
    )
    flags, reply = w.recv()
    elapsed = time.monotonic() - start
    w.close()
    if not reply.get("ok"):
        return ("error", reply.get("code"), reply.get("errmsg"), bool(flags & MORE_TO_COME))
    return ("ok", held(elapsed), bool(flags & MORE_TO_COME))


def stream_count(uri: str, seconds: float = 2.2) -> int:
    """Replies in `seconds` on an exhaust stream opened with the current topology."""
    w = Wire(uri)
    w.send(
        {
            "hello": 1,
            "topologyVersion": current_topology(uri),
            "maxAwaitTimeMS": BUDGET_MS,
            "$db": "admin",
        },
        flags=EXHAUST_ALLOWED,
    )
    w.sock.settimeout(0.1)
    start, count = time.monotonic(), 0
    while time.monotonic() - start < seconds:
        try:
            w.recv()
        except TimeoutError:
            continue
        count += 1
    w.close()
    return count


def timing_rows(uri: str, with_older: bool) -> list[tuple[str, Any]]:
    tv = current_topology(uri)
    pid, counter = tv["processId"], tv["counter"]
    shapes = {
        "current": tv,
        "another processId": {"processId": ObjectId(), "counter": counter},
        "another processId, newer counter": {
            "processId": ObjectId(),
            "counter": Int64(counter + 5),
        },
        "newer counter": {"processId": pid, "counter": Int64(counter + 1)},
    }
    if with_older:
        shapes["older counter"] = {"processId": pid, "counter": Int64(counter - 1)}
    rows = []
    for exhaust in (False, True):
        for label, shape in shapes.items():
            reply = one_reply(uri, shape, exhaust)
            # The counter is the server's own; blank it so two servers compare.
            if reply[0] == "error":
                reply = (
                    reply[0],
                    reply[1],
                    reply[2]
                    .replace(f"counter: {counter + 1} ", "counter: <n+1> ")
                    .replace(f"counter: {counter}", "counter: <n>"),
                    reply[3],
                )
            rows.append((f"{'exhaust' if exhaust else 'plain'} {label}", reply))
    rows.append(("exhaust stream replies in 2.2s", stream_count(uri)))
    return rows


def parse_rows(uri: str) -> list[tuple[str, Any]]:
    client = pymongo.MongoClient(uri, directConnection=True)
    tv = client.admin.command("hello")["topologyVersion"]
    pid, counter = tv["processId"], tv["counter"]
    shapes: list[tuple[str, dict[str, Any]]] = [
        ("topologyVersion int", {"topologyVersion": 1, "maxAwaitTimeMS": 1}),
        ("topologyVersion string", {"topologyVersion": "x", "maxAwaitTimeMS": 1}),
        ("topologyVersion {}", {"topologyVersion": {}, "maxAwaitTimeMS": 1}),
        ("no processId", {"topologyVersion": {"counter": counter}, "maxAwaitTimeMS": 1}),
        ("no counter", {"topologyVersion": {"processId": pid}, "maxAwaitTimeMS": 1}),
        (
            "processId string",
            {"topologyVersion": {"processId": "x", "counter": counter}, "maxAwaitTimeMS": 1},
        ),
        (
            "int32 counter",
            {"topologyVersion": {"processId": pid, "counter": 0}, "maxAwaitTimeMS": 1},
        ),
        (
            "double counter",
            {"topologyVersion": {"processId": pid, "counter": 1.5}, "maxAwaitTimeMS": 1},
        ),
        (
            "unknown field",
            {
                "topologyVersion": {"processId": pid, "counter": counter, "z": 1},
                "maxAwaitTimeMS": 1,
            },
        ),
        (
            "negative counter",
            {"topologyVersion": {"processId": pid, "counter": Int64(-1)}, "maxAwaitTimeMS": 1},
        ),
        ("topologyVersion null", {"topologyVersion": None, "maxAwaitTimeMS": 1}),
        ("topologyVersion alone", {"topologyVersion": tv}),
        ("maxAwaitTimeMS alone", {"maxAwaitTimeMS": 1}),
        ("maxAwaitTimeMS null", {"topologyVersion": tv, "maxAwaitTimeMS": None}),
        ("both null", {"topologyVersion": None, "maxAwaitTimeMS": None}),
        ("maxAwaitTimeMS -1", {"topologyVersion": tv, "maxAwaitTimeMS": -1}),
        ("maxAwaitTimeMS -0.5", {"topologyVersion": tv, "maxAwaitTimeMS": -0.5}),
        ("maxAwaitTimeMS 1.5", {"topologyVersion": tv, "maxAwaitTimeMS": 1.5}),
        ("maxAwaitTimeMS decimal", {"topologyVersion": tv, "maxAwaitTimeMS": Decimal128("2")}),
        ("maxAwaitTimeMS string", {"topologyVersion": tv, "maxAwaitTimeMS": "x"}),
        ("maxAwaitTimeMS bool", {"topologyVersion": tv, "maxAwaitTimeMS": True}),
        (
            "bad type order",
            {"maxAwaitTimeMS": "x", "topologyVersion": {"counter": 1.5}},
        ),
        (
            "bad type order reversed",
            {"topologyVersion": {"counter": 1.5}, "maxAwaitTimeMS": "x"},
        ),
        ("isMaster path", {"isMaster": 1, "topologyVersion": 1, "maxAwaitTimeMS": 1}),
    ]
    rows = []
    for label, extra in shapes:
        cmd = extra if "isMaster" in extra else {"hello": 1, **extra}
        try:
            client.admin.command(cmd)
            rows.append((label, ("ok",)))
        except pymongo.errors.OperationFailure as e:
            rows.append((label, (e.code, (e.details or {}).get("errmsg"))))
    client.close()
    return rows


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD and PROBE_SERVER are required (see the module docstring)")
        return 2
    # Self-check: mongod must hold a current client, or the box is too loaded for
    # the timing half to mean anything.
    if one_reply(MONGOD, current_topology(MONGOD), False)[1] != "held":
        print("SELF-CHECK FAILED: mongod did not hold a current awaitable hello")
        return 2
    with_older = current_topology(MONGOD)["counter"] > 0 and current_topology(SERVER)["counter"] > 0
    want = timing_rows(MONGOD, with_older) + parse_rows(MONGOD)
    got = timing_rows(SERVER, with_older) + parse_rows(SERVER)
    bad = 0
    for (label, w), (_, g) in zip(want, got, strict=True):
        if w != g:
            bad += 1
            print(f"DIFF {label}\n  mongod: {w}\n  ours:   {g}")
    if not with_older:
        print("  (older-counter case not run: a server's counter is still 0)")
    print(f"=== awaitable hello: {bad} of {len(want)} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
