"""Byte-sortable encoding of BSON values for index keys.

The encoder produces ``bytes`` such that lexicographic byte comparison matches
MongoDB's BSON cross-type sort order (so numbers sort below strings, strings
below objects, etc., and within a type values sort by their natural order).
That lets us put encoded keys in WiredTiger's sorted B-tree and use range
scans for ``$gt`` / ``$gte`` / ``$lt`` / ``$lte`` / ``$in`` and for
sort-by-indexed-field, not just equality.

The encoding is one-way: it preserves order and equality, not the original
value (so we don't decode it back). The original document is still the BSON
blob in ``table:secantus_documents``; the index entry's ``key_bytes`` is just
a sortable fingerprint.

Layout: ``<rank_byte><payload>``. ``rank_byte`` is the BSON type rank
(MinKey=1 .. MaxKey=13). Payload format depends on type — see ``_encode_*``
helpers. Compound keys are joined with ``\\x00\\x00`` after payload nulls
have been escaped to ``\\x00\\xff``, so the join is unambiguous and
byte-sortable.

Numeric encoding (the hard one) uses a Decimal-based "lexical decimal" form
so int / long / double / Decimal128 collide on equal value and sort
correctly across the whole numeric type. Specials (NaN, ±Infinity) get
dedicated bytes that bracket the finite range.
"""

from __future__ import annotations

import datetime as _dt
import math
import struct
from collections.abc import Mapping
from decimal import Decimal, InvalidOperation
from typing import Any

import bson
from bson import Binary, Decimal128, MaxKey, MinKey, ObjectId, Regex, Timestamp

# Type ranks — must match storage._bson_type_rank.
RANK_MINKEY = 1
RANK_NULL = 2
RANK_NUMBER = 3
RANK_STRING = 4
RANK_DOCUMENT = 5
RANK_ARRAY = 6
RANK_BINDATA = 7
RANK_OBJECTID = 8
RANK_BOOL = 9
RANK_DATE = 10
RANK_TIMESTAMP = 11
RANK_REGEX = 12
RANK_MAXKEY = 13


def _rank(value: Any) -> int:
    if isinstance(value, MinKey):
        return RANK_MINKEY
    if value is None:
        return RANK_NULL
    if isinstance(value, bool):
        return RANK_BOOL
    if isinstance(value, (int, float, Decimal128)):
        return RANK_NUMBER
    if isinstance(value, str):
        return RANK_STRING
    if isinstance(value, Mapping):
        return RANK_DOCUMENT
    if isinstance(value, list):
        return RANK_ARRAY
    if isinstance(value, (bytes, Binary, bytearray)):
        return RANK_BINDATA
    if isinstance(value, ObjectId):
        return RANK_OBJECTID
    if isinstance(value, _dt.datetime):
        return RANK_DATE
    if isinstance(value, Timestamp):
        return RANK_TIMESTAMP
    if isinstance(value, Regex):
        return RANK_REGEX
    if isinstance(value, MaxKey):
        return RANK_MAXKEY
    return RANK_DOCUMENT  # unknown -> treat as object


# Byte-sortable decimal markers within the number payload.
_NUM_NAN = b"\x00"
_NUM_NEG_INF = b"\x20"
_NUM_NEG = 0x40  # prefix byte for finite negative
_NUM_ZERO = b"\x80"
_NUM_POS = 0xC0  # prefix byte for finite positive
_NUM_POS_INF = b"\xff"


def _to_decimal(value: int | float | Decimal128) -> Decimal | None:
    """Return value as a Decimal, or None for NaN/Infinity."""
    if isinstance(value, Decimal128):
        try:
            d = value.to_decimal()
        except (InvalidOperation, ValueError):
            return None
        if not d.is_finite():
            return None
        return d
    if isinstance(value, float):
        if math.isnan(value) or math.isinf(value):
            return None
        try:
            return Decimal(repr(value))
        except (InvalidOperation, ValueError):
            return None
    return Decimal(int(value))


def _is_nan(value: int | float | Decimal128) -> bool:
    if isinstance(value, float):
        return math.isnan(value)
    if isinstance(value, Decimal128):
        try:
            return value.to_decimal().is_nan()
        except (InvalidOperation, ValueError):
            return False
    return False


def _inf_sign(value: int | float | Decimal128) -> int:
    """Return +1 for +inf, -1 for -inf, 0 if not infinity."""
    if isinstance(value, float):
        if value == math.inf:
            return 1
        if value == -math.inf:
            return -1
        return 0
    if isinstance(value, Decimal128):
        try:
            d = value.to_decimal()
        except (InvalidOperation, ValueError):
            return 0
        if d.is_infinite():
            return -1 if d.is_signed() else 1
    return 0


def _encode_number(value: int | float | Decimal128) -> bytes:
    if _is_nan(value):
        return _NUM_NAN
    inf = _inf_sign(value)
    if inf > 0:
        return _NUM_POS_INF
    if inf < 0:
        return _NUM_NEG_INF
    d = _to_decimal(value)
    if d is None or d.is_zero():
        return _NUM_ZERO
    sign = 1 if d > 0 else -1
    d = abs(d).normalize()
    digits, exp = d.as_tuple()[1:3]
    sci_exp = exp + len(digits) - 1
    bias_e = 128 + sci_exp
    if not 0 <= bias_e <= 255:
        # Out of single-byte exponent range — fall back to a marker that
        # still sorts on the correct side of zero. Magnitudes within ranks
        # collapse together; acceptable until we widen the encoding.
        return bytes([_NUM_POS, 0xFF, 0xFF]) if sign > 0 else bytes([_NUM_NEG, 0x00, 0x00])
    if sign < 0:
        bias_e = 0xFF - bias_e
    if len(digits) % 2 == 1:
        digits = digits + (0,)
    pairs = bytes(da * 10 + db + 1 for da, db in zip(digits[::2], digits[1::2], strict=True))
    if sign < 0:
        pairs = bytes(0x64 - p for p in pairs)  # invert: 100 - encoded
    prefix = _NUM_POS if sign > 0 else _NUM_NEG
    terminator = b"\x00" if sign > 0 else b"\xff"
    return bytes([prefix, bias_e]) + pairs + terminator


def _escape(data: bytes) -> bytes:
    """Replace 0x00 bytes so 0x00 0x00 can be a compound separator."""
    return data.replace(b"\x00", b"\x00\xff")


def _encode_string(s: str, collation: Any = None) -> bytes:
    """UTF-8 byte-sortable encoding for a string, optionally
    collation-normalised.

    When ``collation`` is set and supports index encoding, the string
    is normalised (accents stripped / case-folded per the collation's
    ``strength`` and ``case_level``) before encoding so the entries
    table's lex byte order matches the collation's sort order. Index
    entries are written with the SAME collation that queries use to
    look them up, so two strings that compare-equal under the
    collation produce the same key bytes and hit the same row.

    ``numericOrdering`` is intentionally not supported at the index
    level (would need a length-prefixed digit-run encoding for
    sortability); queries that combine ``numericOrdering`` with an
    index fall back to COLLSCAN per the picker's contract.
    """
    if collation is not None and getattr(collation, "supports_index_encoding", False):
        # Local import — sortkey.py is on the import-cycle floor;
        # collation.py imports sortkey via storage, so we can't
        # top-import.
        from secantus.collation import normalize_for_index_bytes

        return _escape(normalize_for_index_bytes(s, collation))
    return _escape(s.encode("utf-8"))


def _encode_binary(b: bytes | Binary | bytearray) -> bytes:
    raw = bytes(b)
    return struct.pack(">I", len(raw)) + _escape(raw)


def _encode_objectid(oid: ObjectId) -> bytes:
    return oid.binary  # 12 bytes, already byte-sortable


def _encode_date(d: _dt.datetime) -> bytes:
    if d.tzinfo is not None:
        ms = int(d.timestamp() * 1000)
    else:
        epoch = _dt.datetime(1970, 1, 1)
        ms = int((d - epoch).total_seconds() * 1000)
    return _signed_int64_sortable(ms)


def _signed_int64_sortable(n: int) -> bytes:
    # Map signed int64 to unsigned by flipping the top bit, then pack big-endian.
    # Negatives map below zero (top-bit-cleared), positives map above (top-bit-set).
    n &= 0xFFFFFFFFFFFFFFFF  # truncate just in case
    return struct.pack(">Q", n ^ 0x8000000000000000)


def _encode_timestamp(ts: Timestamp) -> bytes:
    return struct.pack(">II", ts.time, ts.inc)


def _encode_doc(doc: Mapping[str, Any]) -> bytes:
    return _escape(bson.encode(dict(doc)))


def _encode_array(arr: list[Any]) -> bytes:
    # Encode as a BSON document with positional keys — this matches how
    # BSON itself stores arrays, so equality through the index lines up
    # with equality at the matches() layer.
    return _escape(bson.encode({str(i): v for i, v in enumerate(arr)}))


def _encode_regex(r: Regex) -> bytes:
    pattern = r.pattern.encode("utf-8") if isinstance(r.pattern, str) else bytes(r.pattern)
    flags = r.flags.encode("utf-8") if isinstance(r.flags, str) else bytes(r.flags)
    return _escape(pattern) + b"\x00\x00" + _escape(flags)


def encode_value(value: Any, *, collation: Any = None) -> bytes:
    """Single-value byte-sortable BSON encoding.

    When ``collation`` is set and the value is a string, the string
    is normalised via :func:`secantus.collation.normalize_for_index_bytes`
    before encoding so the resulting bytes sort by the collation's
    rules rather than raw codepoint. Non-string values pass through
    unchanged. Used by index-write and index-lookup paths to keep
    string entries collation-aware.
    """
    rank = _rank(value)
    head = bytes([rank])
    if rank in (RANK_MINKEY, RANK_NULL, RANK_MAXKEY):
        return head
    if rank == RANK_NUMBER:
        return head + _encode_number(value)
    if rank == RANK_STRING:
        return head + _encode_string(value, collation)
    if rank == RANK_DOCUMENT:
        return head + _encode_doc(value)
    if rank == RANK_ARRAY:
        return head + _encode_array(value)
    if rank == RANK_BINDATA:
        return head + _encode_binary(value)
    if rank == RANK_OBJECTID:
        return head + _encode_objectid(value)
    if rank == RANK_BOOL:
        return head + (b"\x01" if value else b"\x00")
    if rank == RANK_DATE:
        return head + _encode_date(value)
    if rank == RANK_TIMESTAMP:
        return head + _encode_timestamp(value)
    if rank == RANK_REGEX:
        return head + _encode_regex(value)
    return head  # unreachable


COMPOUND_SEP = b"\x00\x00"


def encode_compound(values: list[Any], *, collation: Any = None) -> bytes:
    """Compound key. Components are null-escaped; ``\\x00\\x00`` separates.

    ``collation`` applies uniformly to every string component (matches
    mongod — a collation is a per-index property, not per-field).
    """
    return COMPOUND_SEP.join(encode_value(v, collation=collation) for v in values)


def invert_bytes(b: bytes) -> bytes:
    """Bitwise-NOT every byte. Order-reversing: if a < b byte-wise, then ~a > ~b.

    Used to store descending-direction index entries so a forward B-tree
    walk yields values in descending order without per-row reversal.
    """
    return bytes(x ^ 0xFF for x in b)


def encode_value_directed(value: Any, direction: int = 1, *, collation: Any = None) -> bytes:
    """Like ``encode_value`` but inverts bytes when ``direction == -1``."""
    e = encode_value(value, collation=collation)
    return invert_bytes(e) if direction == -1 else e


# Range-query bound helpers. Bounds are returned as (key_bytes, inclusive)
# tuples so the WT range scan can apply them with the right boundary
# semantics. ``None`` for a bound means open-ended.
def gt_bound(value: Any, *, collation: Any = None) -> tuple[bytes, bool]:
    return encode_value(value, collation=collation), False


def gte_bound(value: Any, *, collation: Any = None) -> tuple[bytes, bool]:
    return encode_value(value, collation=collation), True


def lt_bound(value: Any, *, collation: Any = None) -> tuple[bytes, bool]:
    return encode_value(value, collation=collation), False


def lte_bound(value: Any, *, collation: Any = None) -> tuple[bytes, bool]:
    return encode_value(value, collation=collation), True
