"""RM-836: a stop and continue of the server must not close its connections.

On Linux a read that has a timeout (`SO_RCVTIMEO`) fails with EINTR after the
process is stopped (SIGSTOP, `docker pause`, a debugger) and continued, with no
signal handler installed (signal(7)). `secantusd-mdb` read its sockets with a
250 ms timeout and closed the connection on any error but a timeout, so every
connection, the driver monitors' streaming `hello` included, was dropped on
SIGCONT; mongod's reads carry on. The gap was found by the RM-372 ingress
limiter test, whose pymongo client lost its connection to this server on
Linux CI (PR #3076, run 45985326929) while the server was stopped for the
milliseconds six connections took to open.
"""

from __future__ import annotations

import os
import signal
import socket
import struct
import time
from typing import Any

import bson
import pytest

from tests.backlog.conftest import mongod, rust_mongo  # noqa: F401  (fixtures)


def _send(sock: socket.socket, command: dict, flags: int = 0) -> None:
    body = struct.pack("<I", flags) + b"\x00" + bson.encode({**command, "$db": "admin"})
    sock.sendall(struct.pack("<iiii", 16 + len(body), 1, 0, 2013) + body)


def _receive(sock: socket.socket) -> dict | None:
    """The reply document, or `None` when the server closed the connection."""
    try:
        header = sock.recv(16, socket.MSG_WAITALL)
        if len(header) < 16:
            return None
        (length,) = struct.unpack("<i", header[:4])
        return bson.decode(sock.recv(length - 16, socket.MSG_WAITALL)[5:])
    except ConnectionError:
        return None


def _stopped_for(pid: int, seconds: float) -> None:
    os.kill(pid, signal.SIGSTOP)
    try:
        time.sleep(seconds)
    finally:
        os.kill(pid, signal.SIGCONT)


def _survives_a_stop(client: Any) -> dict:
    """What three connections answer after the server is stopped and
    continued: one idle (waiting for its first byte), one between commands,
    and one inside an awaitable `hello` that holds for `maxAwaitTimeMS`."""
    host, port = client.address
    pid = client.admin.command("serverStatus")["pid"]
    socks = [socket.create_connection((host, port), timeout=10) for _ in range(3)]
    idle, between, awaiting = socks
    try:
        _send(between, {"hello": 1})
        first = _receive(between)
        _send(awaiting, {"hello": 1})
        version = _receive(awaiting)["topologyVersion"]
        # As a driver's monitor asks: `exhaustAllowed` (flag bit 16).
        hold = {"hello": 1, "topologyVersion": version, "maxAwaitTimeMS": 3000}
        _send(awaiting, hold, flags=1 << 16)
        time.sleep(0.3)  # the server is holding it, probing the socket
        _stopped_for(pid, 0.5)
        _send(idle, {"hello": 1})
        _send(between, {"hello": 1})
        return {
            "first": bool(first and first.get("ok") == 1.0),
            "idle": (_receive(idle) or {}).get("ok"),
            "between": (_receive(between) or {}).get("ok"),
            "awaiting": (_receive(awaiting) or {}).get("ok"),
        }
    finally:
        for s in socks:
            s.close()


@pytest.mark.backlog_fixed("RM-836")
def test_connections_survive_a_stop_and_continue(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    """mongod 8.2.11 answers all three connections after SIGSTOP and SIGCONT;
    this server must too."""
    assert (
        _survives_a_stop(rust_mongo)
        == _survives_a_stop(mongod)
        == {
            "first": True,
            "idle": 1.0,
            "between": 1.0,
            "awaiting": 1.0,
        }
    )
