"""Postgres ``bytea`` binary type: literal parsing, ``encode`` / ``decode``
format conversions, and the byte-level accessor functions.

A ``bytea`` value is a Python ``bytes`` during evaluation and a ``bson.Binary``
once stored (the round-trip is already handled in ``typemap`` — ``coerce`` wraps
``bytes`` in ``Binary``, ``to_py`` unwraps it, and ``to_pg_text`` renders it as
the ``\\x…`` hex form Postgres emits under the default ``bytea_output = hex``).

Two text input forms are accepted (matching Postgres):

- **hex** — ``\\x`` followed by hex digit pairs (whitespace between pairs is
  ignored): ``'\\xdeadbeef'``.
- **escape** — printable bytes verbatim, a literal backslash doubled (``\\\\``),
  and any other byte as a three-digit octal escape ``\\ooo``: ``'ab\\001c'``.

Out of scope: ``bytea_output = escape`` server setting (output is always hex, as
in modern Postgres), and the SHA/MD5 digest functions (those live with the
crypto extensions, not core ``bytea``).
"""

from __future__ import annotations

import base64
import re
from typing import Any

_HEX_WS_RE = re.compile(r"\s+")


class ByteaError(ValueError):
    """A malformed ``bytea`` literal or an out-of-range byte index."""


def _parse_escape(s: str) -> bytes:
    """Parse a Postgres ``bytea`` *escape*-format string into bytes."""
    out = bytearray()
    i, n = 0, len(s)
    while i < n:
        ch = s[i]
        if ch == "\\":
            if i + 1 < n and s[i + 1] == "\\":  # doubled backslash -> one byte
                out.append(0x5C)
                i += 2
                continue
            if i + 3 < n and s[i + 1 : i + 4].isdigit():  # \ooo octal
                try:
                    out.append(int(s[i + 1 : i + 4], 8))
                except ValueError as exc:
                    raise ByteaError(f"invalid octal escape in bytea: {s[i : i + 4]!r}") from exc
                i += 4
                continue
            raise ByteaError(f"invalid bytea escape at offset {i}: {s[i:]!r}")
        out.append(ord(ch) & 0xFF)
        i += 1
    return bytes(out)


def parse(value: Any) -> bytes:
    """Normalise a ``bytea`` literal to Python ``bytes``.

    Accepts raw ``bytes`` / ``bytearray`` (passed through), a ``\\x``-prefixed hex
    string, or a Postgres escape-format string.
    """
    if isinstance(value, (bytes, bytearray)):
        return bytes(value)
    s = str(value)
    if s[:2] in ("\\x", "\\X"):
        hexed = _HEX_WS_RE.sub("", s[2:])
        try:
            return bytes.fromhex(hexed)
        except ValueError as exc:
            raise ByteaError(f"invalid hex in bytea literal: {value!r}") from exc
    return _parse_escape(s)


def _render_escape(data: bytes) -> str:
    """Render bytes as Postgres ``bytea`` escape-format text."""
    parts: list[str] = []
    for b in data:
        if b == 0x5C:
            parts.append("\\\\")
        elif 0x20 <= b <= 0x7E:
            parts.append(chr(b))
        else:
            parts.append(f"\\{b:03o}")
    return "".join(parts)


def encode(data: Any, fmt: str) -> str:
    """``encode(bytea, fmt)`` — render bytes as text in ``hex`` / ``base64`` /
    ``escape`` format."""
    raw = parse(data)
    f = fmt.lower()
    if f == "hex":
        return raw.hex()
    if f == "base64":
        return base64.b64encode(raw).decode("ascii")
    if f == "escape":
        return _render_escape(raw)
    raise ByteaError(f"unrecognized encoding: {fmt!r}")


def decode(text: Any, fmt: str) -> bytes:
    """``decode(text, fmt)`` — parse text in ``hex`` / ``base64`` / ``escape``
    format into bytes."""
    s = str(text)
    f = fmt.lower()
    if f == "hex":
        try:
            return bytes.fromhex(_HEX_WS_RE.sub("", s))
        except ValueError as exc:
            raise ByteaError(f"invalid hex for decode(): {text!r}") from exc
    if f == "base64":
        try:
            return base64.b64decode(s)
        except (ValueError, base64.binascii.Error) as exc:
            raise ByteaError(f"invalid base64 for decode(): {text!r}") from exc
    if f == "escape":
        return _parse_escape(s)
    raise ByteaError(f"unrecognized encoding: {fmt!r}")


def get_byte(data: Any, n: int) -> int:
    """``get_byte(bytea, n)`` — the 0-based ``n``-th byte as an integer."""
    raw = parse(data)
    if n < 0 or n >= len(raw):
        raise ByteaError(f"index {n} out of range 0..{len(raw) - 1}")
    return raw[n]


def set_byte(data: Any, n: int, newvalue: int) -> bytes:
    """``set_byte(bytea, n, v)`` — a copy of the bytes with the ``n``-th byte set."""
    raw = bytearray(parse(data))
    if n < 0 or n >= len(raw):
        raise ByteaError(f"index {n} out of range 0..{len(raw) - 1}")
    raw[n] = int(newvalue) & 0xFF
    return bytes(raw)


def concat(a: Any, b: Any) -> bytes:
    """``bytea || bytea`` — byte concatenation."""
    return parse(a) + parse(b)
