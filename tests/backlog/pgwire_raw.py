"""A raw PostgreSQL wire client for gap tests that need message-level control.

psycopg cannot send a Bind without an Execute, a Parse inside an aborted
transaction without a Sync of its own, or a malformed binary parameter, and
it hides the order of ParameterStatus and CommandComplete. These are the
shapes the pgtest gauge (CockroachDB's `pgwire/testdata/pgtest` corpus) found
the Rust server answering differently from PostgreSQL 15.19, so the gap
tests drive the protocol directly.

`Wire.run(...)` sends a batch of frontend messages and returns what came
back, up to and including the next ReadyForQuery, as small tuples that keep
what a client sees and drop what varies between servers (process ids, table
OIDs, error text).
"""

from __future__ import annotations

import socket
import struct
from typing import Any


def _cstr(s: str) -> bytes:
    return s.encode() + b"\0"


def _msg(kind: bytes, body: bytes = b"") -> bytes:
    return kind + struct.pack("!i", len(body) + 4) + body


def query(sql: str) -> bytes:
    return _msg(b"Q", _cstr(sql))


def parse(sql: str, name: str = "", oids: tuple[int, ...] = ()) -> bytes:
    body = _cstr(name) + _cstr(sql) + struct.pack("!h", len(oids))
    body += b"".join(struct.pack("!I", o) for o in oids)
    return _msg(b"P", body)


def bind(
    stmt: str = "",
    portal: str = "",
    params: tuple[bytes | None, ...] = (),
    param_formats: tuple[int, ...] = (),
    result_formats: tuple[int, ...] = (),
) -> bytes:
    body = _cstr(portal) + _cstr(stmt)
    body += struct.pack("!h", len(param_formats)) + b"".join(
        struct.pack("!h", f) for f in param_formats
    )
    body += struct.pack("!h", len(params))
    for p in params:
        body += struct.pack("!i", -1) if p is None else struct.pack("!i", len(p)) + p
    body += struct.pack("!h", len(result_formats)) + b"".join(
        struct.pack("!h", f) for f in result_formats
    )
    return _msg(b"B", body)


def describe(kind: str, name: str = "") -> bytes:
    return _msg(b"D", kind.encode() + _cstr(name))


def execute(portal: str = "", max_rows: int = 0) -> bytes:
    return _msg(b"E", _cstr(portal) + struct.pack("!i", max_rows))


SYNC = _msg(b"S")


def _fields(body: bytes) -> dict[str, str]:
    out: dict[str, str] = {}
    i = 0
    while body[i] != 0:
        code = chr(body[i])
        end = body.index(b"\0", i + 1)
        out[code] = body[i + 1 : end].decode()
        i = end + 1
    return out


class Wire:
    """One connection, speaking the protocol by hand."""

    def __init__(self, host: str, port: int, user: str, dbname: str = "postgres") -> None:
        self.sock = socket.create_connection((host, port), timeout=10)
        self.buf = b""
        #: Every ErrorResponse's fields, in arrival order (the decoded reply
        #: keeps only the SQLSTATE).
        self.errors: list[dict[str, str]] = []
        params = _cstr("user") + _cstr(user) + _cstr("database") + _cstr(dbname) + b"\0"
        body = struct.pack("!i", 196608) + params
        self.sock.sendall(struct.pack("!i", len(body) + 4) + body)
        self.startup = self._until_ready()
        for kind, *rest in self.startup:
            if kind == "Authentication" and rest[0] != 0:
                raise RuntimeError(f"the probe connects by trust only, got auth request {rest[0]}")

    def close(self) -> None:
        try:
            self.sock.sendall(_msg(b"X"))
        finally:
            self.sock.close()

    def __enter__(self) -> Wire:
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    def _read(self, n: int) -> bytes:
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("the server closed the connection")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def _message(self) -> tuple:
        kind = self._read(1)
        (length,) = struct.unpack("!i", self._read(4))
        body = self._read(length - 4)
        if kind == b"E":
            self.errors.append(_fields(body))
        return _decode(kind, body)

    def _until_ready(self) -> list[tuple]:
        out = []
        while True:
            m = self._message()
            out.append(m)
            if m[0] == "ReadyForQuery":
                return out

    def run(self, *messages: bytes) -> list[tuple]:
        """Send `messages`, return the replies through the next ReadyForQuery."""
        self.sock.sendall(b"".join(messages))
        return self._until_ready()


def _decode(kind: bytes, body: bytes) -> tuple:
    k = kind.decode()
    if k == "R":
        return ("Authentication", struct.unpack("!i", body[:4])[0])
    if k == "S":
        name, value = body.split(b"\0")[:2]
        return ("ParameterStatus", name.decode(), value.decode())
    if k == "K":
        return ("BackendKeyData",)
    if k == "Z":
        return ("ReadyForQuery", body.decode())
    if k == "1":
        return ("ParseComplete",)
    if k == "2":
        return ("BindComplete",)
    if k == "3":
        return ("CloseComplete",)
    if k == "n":
        return ("NoData",)
    if k == "s":
        return ("PortalSuspended",)
    if k == "I":
        return ("EmptyQueryResponse",)
    if k == "C":
        return ("CommandComplete", body.rstrip(b"\0").decode())
    if k == "E":
        return ("ErrorResponse", _fields(body)["C"])
    if k == "N":
        f = _fields(body)
        return ("NoticeResponse", f["V"], f["C"])
    if k == "t":
        (n,) = struct.unpack("!h", body[:2])
        return ("ParameterDescription", struct.unpack(f"!{n}I", body[2 : 2 + 4 * n]))
    if k == "T":
        (n,) = struct.unpack("!h", body[:2])
        i, cols = 2, []
        for _ in range(n):
            end = body.index(b"\0", i)
            name = body[i:end].decode()
            _tbl, attnum, typoid, typlen, _typmod, fmt = struct.unpack(
                "!IhIhih", body[end + 1 : end + 19]
            )
            # The table OID is installation-specific; the attnum is not.
            cols.append((name, attnum, typoid, typlen, fmt))
            i = end + 19
        return ("RowDescription", tuple(cols))
    if k == "D":
        (n,) = struct.unpack("!h", body[:2])
        i, vals = 2, []
        for _ in range(n):
            (ln,) = struct.unpack("!i", body[i : i + 4])
            i += 4
            if ln == -1:
                vals.append(None)
            else:
                vals.append(body[i : i + ln].hex())
                i += ln
        return ("DataRow", tuple(vals))
    if k == "H":
        return ("CopyOutResponse",)
    if k == "d":
        return ("CopyData", body.hex())
    if k == "c":
        return ("CopyDone",)
    return ("?", k)
