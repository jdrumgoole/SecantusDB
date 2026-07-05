"""Postgres bit-string types: ``bit(n)`` (fixed) and ``bit varying(n)`` / ``varbit``.

Values are stored as a canonical string of ``'0'`` / ``'1'`` characters (the same
text Postgres renders), so the storage form *is* the wire form. This module
validates, pads/truncates, and implements the bitwise algebra; ``secantus.sql``
``scalar`` / ``typemap`` / ``planner`` wire it into the SQL surface.

Bit positions for ``get_bit`` / ``set_bit`` count from the **left** (the most
significant bit is index 0), matching Postgres.

Out of scope: two's-complement semantics for ``int::bit`` beyond the low bits,
and any bit-string index.
"""

from __future__ import annotations

from typing import Any


class BitError(ValueError):
    """A malformed bit-string literal or a mismatched-length operation."""


def normalize(text: Any, *, length: int | None = None, varying: bool = False) -> str:
    """Validate a ``'0'``/``'1'`` string and fit it to ``length``.

    A fixed ``bit(n)`` (``varying=False`` with ``length=n``) is right-padded with
    zeros or truncated to exactly ``n`` bits. A ``varbit(n)`` is truncated to at
    most ``n`` bits but never padded; a length-less ``varbit`` is kept as-is."""
    s = str(text).strip()
    if s and not all(c in "01" for c in s):
        raise BitError(f'invalid bit-string value: "{text}" (only 0/1 allowed)')
    if length is not None:
        if len(s) > length:
            s = s[:length]
        elif not varying and len(s) < length:
            s = s + "0" * (length - len(s))
    return s


def from_int(value: int, length: int) -> str:
    """``int::bit(n)`` — the low ``length`` bits of ``value`` (two's complement for
    negatives), most-significant bit first."""
    n = int(value) & ((1 << length) - 1)
    return format(n, f"0{length}b")


def to_int(bits: str) -> int:
    """``bit::int`` — the unsigned integer the bit string denotes."""
    return int(bits, 2) if bits else 0


def _require_same_length(a: str, b: str) -> None:
    if len(a) != len(b):
        raise BitError(f"cannot {a!r} op {b!r}: bit strings of different length")


def band(a: str, b: str) -> str:
    _require_same_length(a, b)
    return "".join("1" if x == "1" and y == "1" else "0" for x, y in zip(a, b, strict=True))


def bor(a: str, b: str) -> str:
    _require_same_length(a, b)
    return "".join("1" if x == "1" or y == "1" else "0" for x, y in zip(a, b, strict=True))


def bxor(a: str, b: str) -> str:
    _require_same_length(a, b)
    return "".join("1" if x != y else "0" for x, y in zip(a, b, strict=True))


def bnot(a: str) -> str:
    return "".join("1" if c == "0" else "0" for c in a)


def shift_left(a: str, n: int) -> str:
    """``a << n`` — preserves width; bits shifted off the left are lost, zeros fill
    on the right."""
    width = len(a)
    if n < 0:
        return shift_right(a, -n)
    if n >= width:
        return "0" * width
    return (a[n:] + "0" * n) if width else a


def shift_right(a: str, n: int) -> str:
    """``a >> n`` — preserves width; bits shifted off the right are lost, zeros fill
    on the left."""
    width = len(a)
    if n < 0:
        return shift_left(a, -n)
    if n >= width:
        return "0" * width
    return ("0" * n + a[: width - n]) if width else a


def concat(a: str, b: str) -> str:
    return a + b


def get_bit(bits: str, n: int) -> int:
    """``get_bit(bits, n)`` — the ``n``'th bit (0 = leftmost)."""
    if n < 0 or n >= len(bits):
        raise BitError(f"bit index {n} out of range for length {len(bits)}")
    return int(bits[n])


def set_bit(bits: str, n: int, value: int) -> str:
    """``set_bit(bits, n, v)`` — a copy with the ``n``'th bit set to ``v``."""
    if n < 0 or n >= len(bits):
        raise BitError(f"bit index {n} out of range for length {len(bits)}")
    return bits[:n] + ("1" if int(value) else "0") + bits[n + 1 :]


def bit_length(bits: str) -> int:
    return len(bits)


def octet_length(bits: str) -> int:
    return (len(bits) + 7) // 8


def is_bit_value(v: Any) -> bool:
    """Whether ``v`` looks like a stored bit string — a non-empty ``'0'``/``'1'``
    string. Used to disambiguate the overloaded ``&`` / ``|`` / ``#`` / ``<<`` /
    ``>>`` operators from their integer forms."""
    return isinstance(v, str) and v != "" and all(c in "01" for c in v)
