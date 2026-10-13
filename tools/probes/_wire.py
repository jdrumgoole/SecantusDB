"""One raw OP_MSG connection, for probes that must say exactly what is sent.

A driver adds a session to every command and picks the transaction fields
itself; a probe of sessions, transactions or cursors has to write them out.
"""

import contextlib
import socket
import struct
from urllib.parse import urlparse

import bson

TIMEOUT = 6.0


class Wire:
    """One OP_MSG connection."""

    def __init__(self, uri):
        parsed = urlparse(uri)
        self.sock = socket.create_connection((parsed.hostname, parsed.port), timeout=TIMEOUT)
        self.request = 0

    def command(self, db, command):
        return self.raw({**command, "$db": db})

    def raw(self, command):
        """Send `command` as it is, with whatever `$db` it has or lacks."""
        self.request += 1
        body = bson.encode(command)
        payload = struct.pack("<I", 0) + b"\x00" + body
        header = struct.pack("<iiii", 16 + len(payload), self.request, 0, 2013)
        self.sock.sendall(header + payload)
        head = self._read(16)
        (length, _, _, _) = struct.unpack("<iiii", head)
        rest = self._read(length - 16)
        return bson.decode(rest[5:])

    def _read(self, n):
        out = b""
        while len(out) < n:
            chunk = self.sock.recv(n - len(out))
            if not chunk:
                raise ConnectionError("closed")
            out += chunk
        return out

    def close(self):
        with contextlib.suppress(OSError):
            self.sock.close()
