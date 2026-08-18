"""Extended query protocol: prepared statements, portals, and bound parameters.

The simple ``Query`` path (P1) parses and runs SQL text in one shot. The
extended protocol splits that into Parse → Bind → Describe → Execute → Sync, so
a client can prepare a statement once and run it many times with different
``$1`` parameters. This is what psycopg / JDBC use.

``ExtendedSession`` holds the per-connection prepared-statement and portal
registries and a state machine that processes one frontend message at a time,
returning the bytes to send back. On error it enters the protocol's
"skip until Sync" state, so a failed statement doesn't desync the stream.
"""

from __future__ import annotations

import contextlib
import datetime as _dt
import decimal
import ipaddress as _ipaddress
import logging
import os
import struct
import uuid as _uuid
from dataclasses import dataclass, field
from decimal import Decimal
from typing import Any

import bson
from sqlglot import exp

from secantus.sql import copyfmt, engine, errors, pggeo, pgwire, planner, typemap
from secantus.sql.catalog import ENUM_TYPE_OID_BASE, USER_TYPE_ARRAY_OID_OFFSET, Catalog
from secantus.sql.session import Session

logger = logging.getLogger(__name__)


@dataclass
class Prepared:
    name: str
    stmt: exp.Expression | None  # None for an empty query string
    param_oids: list[int]
    param_count: int
    # The original query text and creation time — pg_prepared_statements
    # reports both (psycopg's prepared-statement cache matches on the text).
    query: str = ""
    created: Any = None
    # The result shape (name, oid) captured at this statement's first
    # execution — PG's "cached plan" identity. A later execution whose shape
    # differs (DDL changed the table under a SELECT *) raises `cached plan
    # must not change result type` (0A000) until the client re-prepares,
    # exactly like PG. The ErrorResponse carries ROUTINE=RevalidateCachedQuery
    # because pgjdbc's transparent re-prepare-and-retry (willHealViaReparse)
    # matches on that field, not the SQLSTATE.
    plan_shape: list[tuple[str, int]] | None = None


@dataclass
class Portal:
    name: str
    prepared: Prepared
    params: list[Any]
    bound_stmt: exp.Expression | None = None
    result: Any = None  # SQLResult, computed lazily at Execute
    offset: int = 0
    executed: bool = False
    # Bind's result-format codes (0=text, 1=binary): [] all-text, [c] all-c, else per-col.
    result_formats: list[int] = field(default_factory=list)
    #: id() of the explicit transaction handle this portal was bound inside,
    #: or None. Re-binding a NAMED portal still live in the SAME explicit
    #: transaction is PG's 42P03 (pgtest multiple_active_portals); portals
    #: from other/implicit cycles keep the permissive replace this server
    #: has always done (clients re-use portal names across Sync cycles).
    txn_token: int | None = None


# Postgres binary timestamps count microseconds from 2000-01-01 00:00:00 UTC;
# dates count days from the same epoch.
_PG_EPOCH = _dt.datetime(2000, 1, 1, tzinfo=_dt.timezone.utc)
_PG_EPOCH_DATE = _dt.date(2000, 1, 1)


def _decode_numeric(b: bytes) -> Decimal:
    """Decode Postgres' binary ``numeric`` (base-10000 digits)."""
    ndigits, weight, sign, dscale = struct.unpack_from("!HhHH", b, 0)
    if dscale > 0x3FFF:
        # PG's numeric_recv rejects a scale outside NUMERIC_DSCALE_MASK —
        # the pgtest decimal corpus sends dscale=0xFFF0 (a negative int16)
        # and expects 22P03 at Bind.
        raise errors.SQLError("22P03", 'invalid scale in external "numeric" value')
    digits = [struct.unpack_from("!H", b, 8 + 2 * i)[0] for i in range(ndigits)]
    if sign == 0xC000:  # NaN
        return Decimal("NaN")
    if sign == 0xD000:  # +Infinity
        return Decimal("Infinity")
    if sign == 0xF000:  # -Infinity
        return Decimal("-Infinity")
    s = "".join(f"{d:04d}" for d in digits) or "0"
    # The first digit group sits at base-10000 position ``weight``; give the
    # context enough precision for arbitrarily wide values — the integer span
    # is ``(weight+1)*4`` digits, plus ``dscale`` fractional. The default 28
    # significant digits silently rounds, and an under-sized context makes the
    # final quantize raise InvalidOperation on a wide value.
    span = max((weight + 1) * 4, len(s)) + dscale + 4
    dctx = decimal.Context(prec=max(span, 40))
    value = dctx.scaleb(Decimal(s), (weight - (ndigits - 1)) * 4) if digits else Decimal(0)
    if sign == 0x4000:
        value = value.copy_negate()  # context-free: ``-value`` rounds to context prec
    # Round to the declared display scale so 19.99 doesn't become 19.9900...
    # — ALWAYS, including dscale=0: a zero built from N zero digit-groups
    # carries exponent -4N (scaleb of 0 keeps it), and skipping the quantize
    # rendered 0 as 0.000…0 (pgtest decimal:29, the 8192-group regression).
    return value.quantize(Decimal(1).scaleb(-dscale), context=dctx)


def _decode_timestamptz(b: bytes) -> _dt.datetime:
    return _PG_EPOCH + _dt.timedelta(microseconds=struct.unpack("!q", b)[0])


def _decode_timestamp(b: bytes) -> _dt.datetime:
    """Binary ``timestamp`` (without time zone) — same layout, but tz-naive."""
    return _decode_timestamptz(b).replace(tzinfo=None)


def _micros_to_time_text(micros: int) -> str:
    secs, frac = divmod(micros, 1_000_000)
    hh, rem = divmod(secs, 3600)
    mm, ss = divmod(rem, 60)
    out = f"{hh:02d}:{mm:02d}:{ss:02d}"
    if frac:
        out += f".{frac:06d}".rstrip("0")
    return out


def _decode_time(b: bytes) -> str:
    """Binary ``time`` — int64 microseconds since midnight → canonical text."""
    return _micros_to_time_text(struct.unpack("!q", b)[0])


def _decode_timetz(b: bytes) -> str:
    """Binary ``timetz`` — micros since midnight + zone (seconds west of UTC)."""
    micros, zone = struct.unpack("!qi", b)
    offset = -zone  # PG counts west positive; ISO offsets count east positive
    sign = "+" if offset >= 0 else "-"
    oh, om = divmod(abs(offset) // 60, 60)
    return f"{_micros_to_time_text(micros)}{sign}{oh:02d}:{om:02d}"


def _decode_interval(b: bytes) -> dict:
    """Binary ``interval`` — (micros int64, days int32, months int32) → the
    interval subdocument (the typed value casts and arithmetic compare against;
    the old text form silently compared str vs subdoc → always false)."""
    from secantus.sql import intervals as _intervals

    micros, days, months = struct.unpack("!qii", b)
    return _intervals.make(months, days, micros)


def _decode_inet(b: bytes) -> str:
    """Binary ``inet`` / ``cidr`` — family, bits, is_cidr, nbytes, address.
    Malformed payloads get PG's error classes (pgtest inet corpus): a
    truncated header is 08P01; a bad family or address length is 22P03."""
    if len(b) < 4:
        raise errors.SQLError("08P01", "insufficient data left in message")
    family, bits, _is_cidr, nb = struct.unpack_from("!BBBB", b, 0)
    if family == 2:  # PGSQL_AF_INET
        expected = 4
    elif family == 3:  # PGSQL_AF_INET6
        expected = 16
    else:
        raise errors.SQLError("22P03", 'invalid address family in external "inet" value')
    if nb != expected or len(b) < 4 + nb:
        raise errors.SQLError("22P03", 'invalid length in external "inet" value')
    addr = _ipaddress.ip_address(bytes(b[4 : 4 + nb]))
    return f"{addr}/{bits}"


def _decode_macaddr(b: bytes) -> str:
    return ":".join(f"{x:02x}" for x in bytes(b))


def _decode_uuid(b: bytes) -> str:
    return str(_uuid.UUID(bytes=bytes(b)))


def _decode_jsonb(b: bytes) -> str:
    """Binary ``jsonb`` — a 1-byte version header, then JSON text. Wrapped as
    ``JsonText`` so substitution casts it back to a parsed JSON value. An
    empty payload (no version byte) or an unknown version is rejected like
    real PG — the pgtest corpus pins both as errors, not silent acceptance."""
    if len(b) < 1:
        raise errors.SQLError("08P01", "insufficient data left in message")
    if b[0] != 1:
        raise errors.SQLError("08P01", f"unsupported jsonb version number {b[0]}")
    try:
        return typemap.JsonText(bytes(b[1:]).decode("utf-8"))
    except UnicodeDecodeError as e:
        raise errors.SQLError("22021", 'invalid byte sequence for encoding "UTF8"') from e


# Range/multirange binary flags (Postgres' rangetypes.h).
_RANGE_EMPTY = 0x01
_RANGE_LB_INC = 0x02
_RANGE_UB_INC = 0x04
_RANGE_LB_INF = 0x08
_RANGE_UB_INF = 0x10

# Range type OID -> element type OID.
_RANGE_ELEM_OID = {3904: 23, 3906: 1700, 3908: 1114, 3910: 1184, 3912: 1082, 3926: 20}
# Multirange type OID -> its range type OID.
_MULTIRANGE_RANGE_OID = {4451: 3904, 4532: 3906, 4533: 3908, 4534: 3910, 4535: 3912, 4536: 3926}


def _bound_text(value: Any) -> str:
    """Render a decoded range bound as a (quoted where needed) literal token."""
    if isinstance(value, _dt.datetime):
        text = value.isoformat(sep=" ")
    elif isinstance(value, _dt.date):
        text = value.isoformat()
    else:
        text = str(value)
    return f'"{text}"' if " " in text else text


def _decode_range(raw: bytes, elem_oid: int) -> str:
    """Decode a binary range parameter to its text literal (``[a,b)`` / ``empty``),
    which rides the existing range text parser."""
    flags = raw[0]
    if flags & _RANGE_EMPTY:
        return "empty"
    decoder = (lambda b: b.decode("utf-8")) if elem_oid in (0, 25, 1043) else _BINARY[elem_oid]
    off = 1
    bounds: list[str] = []
    for inf_flag in (_RANGE_LB_INF, _RANGE_UB_INF):
        if flags & inf_flag:
            bounds.append("")
            continue
        (length,) = struct.unpack_from("!i", raw, off)
        off += 4
        bounds.append(_bound_text(decoder(raw[off : off + length])))
        off += length
    lb = "[" if flags & _RANGE_LB_INC else "("
    ub = "]" if flags & _RANGE_UB_INC else ")"
    return f"{lb}{bounds[0]},{bounds[1]}{ub}"


def _decode_multirange(raw: bytes, range_oid: int) -> str:
    """Decode a binary multirange parameter to its text literal ``{[a,b),…}``."""
    (count,) = struct.unpack_from("!i", raw, 0)
    elem_oid = _RANGE_ELEM_OID[range_oid]
    off = 4
    parts: list[str] = []
    for _ in range(count):
        (length,) = struct.unpack_from("!i", raw, off)
        off += 4
        parts.append(_decode_range(raw[off : off + length], elem_oid))
        off += length
    return "{" + ",".join(parts) + "}"


def _f8s(raw: bytes, count: int, off: int = 0) -> tuple[float, ...]:
    return struct.unpack_from(f"!{count}d", raw, off)


def _decode_path(raw: bytes) -> str:
    """``path`` — a closed-flag byte, an int32 point count, then the points. The
    closed spelling uses ``(…)`` and the open one ``[…]``."""
    closed = raw[0] != 0
    (npts,) = struct.unpack_from("!i", raw, 1)
    pts = _f8s(raw, 2 * npts, 5)
    body = ",".join(f"({pts[i]},{pts[i + 1]})" for i in range(0, 2 * npts, 2))
    return f"({body})" if closed else f"[{body}]"


def _decode_polygon(raw: bytes) -> str:
    (npts,) = struct.unpack_from("!i", raw, 0)
    pts = _f8s(raw, 2 * npts, 4)
    return "(" + ",".join(f"({pts[i]},{pts[i + 1]})" for i in range(0, 2 * npts, 2)) + ")"


# Geometric binary layouts (Postgres' ``*_send``): fixed runs of float8, except
# ``path`` / ``polygon`` which are length-prefixed. Each decodes to the type's
# text spelling and then through ``pggeo.canonical`` for the tag, so a binary
# parameter and a text one reach storage in exactly the same form.
_GEO_BINARY: dict[int, Any] = {
    600: lambda b: "({},{})".format(*_f8s(b, 2)),  # point
    601: lambda b: "[({},{}),({},{})]".format(*_f8s(b, 4)),  # lseg
    602: _decode_path,  # path
    603: lambda b: "({},{}),({},{})".format(*_f8s(b, 4)),  # box
    604: _decode_polygon,  # polygon
    628: lambda b: "{{{},{},{}}}".format(*_f8s(b, 3)),  # line — coefficients A,B,C
    718: lambda b: "<({},{}),{}>".format(*_f8s(b, 3)),  # circle
}


# Binary parameter decoders by Postgres type OID. The text format (fmt 0) decodes
# to str and rides column-type coercion; libpq clients (psycopg) send many types
# in binary. Types whose storage form is canonical text (time / inet / uuid /
# interval / ranges …) decode to that text and ride the same coercion path.
def _encode_int2vector(v: Any) -> bytes:
    """Binary ``int2vector`` — the wire form is an int2 ARRAY (elemoid 21,
    lower bound 1); the stored form is the space-separated text ("0", "1 2").
    Binary pgwire clients decode pg_index.indoption/indkey through this
    (pgtest int2vector corpus; crdb #111907 shipped int8 elements once)."""
    vals = [int(x) for x in v] if isinstance(v, (list, tuple)) else [int(x) for x in str(v).split()]
    if not vals:
        return struct.pack("!iii", 0, 0, 21)
    out = bytearray(struct.pack("!iiiii", 1, 0, 21, len(vals), 1))
    for x in vals:
        out += struct.pack("!ih", 2, x)
    return bytes(out)


def _decode_oid(b: bytes) -> int:
    """Binary ``oid`` (and the reg* pseudo-types): a 4-byte unsigned int. A
    payload of any other length is a protocol violation, like PG's oidrecv
    (pgtest oid corpus sends a 2-byte and a 6-byte one)."""
    if len(b) != 4:
        raise errors.SQLError("08P01", "insufficient data left in message")
    return struct.unpack("!I", b)[0]


def _decode_varbit(b: bytes) -> str:
    """Binary bit / varbit (PG's varbit_recv): an int32 bit length followed by
    ceil(bits/8) data bytes, decoded to the canonical '0'/'1' string. Reading
    past the parameter buffer is 08P01 (insufficient data — an empty binary
    param can't even hold the 4-byte length); leaving bytes unconsumed is 22P03
    ("incorrect binary data format in bind parameter"), matching PG's binary
    bind-parameter framing (pgtest varbit corpus)."""
    if len(b) < 4:
        raise errors.SQLError("08P01", "insufficient data left in message")
    bitlen = struct.unpack("!i", b[:4])[0]
    if bitlen < 0:
        raise errors.SQLError("22P03", "invalid length in external bit string")
    nbytes = (bitlen + 7) // 8
    if len(b) - 4 < nbytes:
        raise errors.SQLError("08P01", "insufficient data left in message")
    if len(b) - 4 != nbytes:
        raise errors.SQLError("22P03", "incorrect binary data format in bind parameter")
    bits = "".join(f"{byte:08b}" for byte in b[4 : 4 + nbytes])
    return bits[:bitlen]


def _decode_char1(b: bytes) -> str | None:
    # "char" binary form is the raw byte(s). Empty / zero byte is the NULL
    # surrogate (the pgtest char corpus reads both back as SQL NULL).
    if b in (b"", b"\x00"):
        return None
    try:
        s = b.decode("utf-8")
    except UnicodeDecodeError:
        s = b.decode("latin-1")
    return s[0]


_BINARY = {
    16: lambda b: b == b"\x01",  # bool
    17: lambda b: bytes(b),  # bytea
    18: _decode_char1,  # "char" (one byte)
    1560: _decode_varbit,  # bit(n)
    1562: _decode_varbit,  # varbit / bit varying
    90008: lambda b: b.decode("utf-8"),  # citext — binary form is the text
    4072: lambda b: b[1:].decode("utf-8"),  # jsonpath: skip the version byte
    90010: lambda b: b[1:].decode("utf-8"),  # ltree: version byte + text
    20: lambda b: struct.unpack("!q", b)[0],  # int8
    21: lambda b: struct.unpack("!h", b)[0],  # int2
    23: lambda b: struct.unpack("!i", b)[0],  # int4
    25: lambda b: b.decode("utf-8"),  # text
    26: lambda b: _decode_oid(b),  # oid (unsigned)
    # reg* pseudo-types ride the oid wire form: a 4-byte unsigned integer.
    24: lambda b: _decode_oid(b),  # regproc
    2202: lambda b: _decode_oid(b),  # regprocedure
    2205: lambda b: _decode_oid(b),  # regclass
    2206: lambda b: _decode_oid(b),  # regtype
    4089: lambda b: _decode_oid(b),  # regnamespace
    4096: lambda b: _decode_oid(b),  # regrole
    114: lambda b: typemap.JsonText(b.decode("utf-8")),  # json — binary form is the text
    700: lambda b: struct.unpack("!f", b)[0],  # float4
    701: lambda b: struct.unpack("!d", b)[0],  # float8
    829: _decode_macaddr,  # macaddr
    869: _decode_inet,  # inet
    650: _decode_inet,  # cidr — same wire layout
    1043: lambda b: b.decode("utf-8"),  # varchar
    1082: lambda b: _PG_EPOCH_DATE + _dt.timedelta(days=struct.unpack("!i", b)[0]),  # date
    1083: _decode_time,  # time
    1114: _decode_timestamp,  # timestamp (no tz)
    1184: _decode_timestamptz,  # timestamptz
    1186: _decode_interval,  # interval
    1266: _decode_timetz,  # timetz
    1700: _decode_numeric,  # numeric
    2950: _decode_uuid,  # uuid
    3802: _decode_jsonb,  # jsonb
}
_BINARY.update(
    {
        oid: (
            lambda b, _d=dec, _t=typemap.OID_TO_TAG[oid]: typemap.TaggedText(
                pggeo.canonical(_d(b), _t), _t
            )
        )
        for oid, dec in _GEO_BINARY.items()
    }
)
_BINARY.update(
    {
        oid: (
            lambda b, _e=elem, _t=typemap.OID_TO_TAG[oid]: typemap.TaggedText(
                _decode_range(b, _e), _t
            )
        )
        for oid, elem in _RANGE_ELEM_OID.items()
    }
)
_BINARY.update(
    {
        oid: (
            lambda b, _r=rng, _t=typemap.OID_TO_TAG[oid]: typemap.TaggedText(
                _decode_multirange(b, _r), _t
            )
        )
        for oid, rng in _MULTIRANGE_RANGE_OID.items()
    }
)

# OID -> range/multirange tag (base types and their array forms) — parameters
# declared with these travel as TaggedText and substitute as ``::tag`` casts.
_RANGEISH_TAG_BY_OID: dict[int, str] = {}
for _tag in (*typemap._RANGE_TAGS, *typemap._MULTIRANGE_TAGS):
    _RANGEISH_TAG_BY_OID[typemap.PG_OID[_tag]] = _tag
    _arr = typemap._ARRAY_PG_OID.get(_tag)
    if _arr is not None:
        _RANGEISH_TAG_BY_OID[_arr] = f"{_tag}[]"


def _encode_timestamptz(value: Any) -> bytes:
    if isinstance(value, str):
        from secantus.sql.datetimes import (
            datetime_sentinel,
            parse_iso_datetime,
            wide_timestamp_micros,
            wide_timestamp_text,
        )

        # ``infinity`` / ``-infinity`` map onto PG's int64 wire sentinels.
        sentinel = datetime_sentinel(value)
        if sentinel is not None:
            return struct.pack("!q", 2**63 - 1 if sentinel == "infinity" else -(2**63))
        # A PG-valid timestamp beyond Python's datetime range travels as text;
        # its binary form is the true out-of-range instant (proleptic math).
        if wide_timestamp_text(value) is not None:
            return struct.pack("!q", wide_timestamp_micros(value))
        # Timestamps can reach the encoder as ISO text (a text-format parameter
        # bound to a timestamp-typed column); parse rather than silently sending
        # text bytes in a field the client will parse as binary.
        value = parse_iso_datetime(value)
    if not isinstance(value, _dt.datetime):
        return str(value).encode("utf-8")
    if value.tzinfo is None:
        value = value.replace(tzinfo=_dt.timezone.utc)
    # Integer microsecond arithmetic — float total_seconds() loses µs precision
    # ~8000 years out, nudging 9999-12-31 23:59:59.999999 across the year-10K
    # boundary that clients reject.
    return struct.pack("!q", (value - _PG_EPOCH) // _dt.timedelta(microseconds=1))


def _encode_date(value: Any) -> bytes:
    if isinstance(value, str):
        from secantus.sql.datetimes import datetime_sentinel, wide_date_days, wide_timestamp_text

        sentinel = datetime_sentinel(value)
        if sentinel is not None:
            return struct.pack("!i", 2**31 - 1 if sentinel == "infinity" else -(2**31))
        if wide_timestamp_text(value) is not None:
            return struct.pack("!i", wide_date_days(value))
        # A ``date`` is *stored* as its canonical ``YYYY-MM-DD`` text.
        value = _dt.date.fromisoformat(value.strip()[:10])
    if isinstance(value, _dt.datetime):
        value = value.date()
    if isinstance(value, _dt.date):
        return struct.pack("!i", (value - _PG_EPOCH_DATE).days)
    return str(value).encode("utf-8")


def _encode_bool(value: Any) -> bytes:
    if isinstance(value, str):
        # A text-format bool parameter arrives as "t"/"f" — both truthy as str.
        value = value.strip().lower() in ("t", "true", "yes", "on", "1")
    return b"\x01" if value else b"\x00"


def _time_text_to_micros(text: str) -> tuple[int, int]:
    """Split stored ``HH:MM:SS[.ffffff][±HH:MM]`` text into (micros, offset_secs)."""
    s = text.strip()
    if s.startswith("24:"):
        # Postgres allows ``24:00:00``; datetime.time does not.
        return 24 * 3600 * 1_000_000, 0
    t = _dt.time.fromisoformat(s)
    micros = ((t.hour * 60 + t.minute) * 60 + t.second) * 1_000_000 + t.microsecond
    off = t.utcoffset()
    return micros, int(off.total_seconds()) if off is not None else 0


def _encode_time(value: Any) -> bytes:
    micros, _off = _time_text_to_micros(str(value))
    return struct.pack("!q", micros)


def _encode_timetz(value: Any) -> bytes:
    micros, off = _time_text_to_micros(str(value))
    return struct.pack("!qi", micros, -off)  # PG counts the zone west-positive


def _encode_interval(value: Any) -> bytes:
    from secantus.sql import intervals as _intervals

    if not (isinstance(value, dict) and "interval" in value):
        value = _intervals.parse(str(value))
    months, days, micros = _intervals._fields(value)
    return struct.pack("!qii", micros, days, months)


def _encode_uuid(value: Any) -> bytes:
    return _uuid.UUID(str(value)).bytes


def _encode_inet_factory(is_cidr: bool):
    def _encode(value: Any) -> bytes:
        iface = _ipaddress.ip_interface(str(value))
        packed = iface.ip.packed
        family = 2 if iface.version == 4 else 3  # PGSQL_AF_INET / PGSQL_AF_INET6
        return (
            struct.pack("!BBBB", family, iface.network.prefixlen, int(is_cidr), len(packed))
            + packed
        )

    return _encode


def _encode_macaddr(value: Any) -> bytes:
    return bytes.fromhex(str(value).replace(":", "").replace("-", ""))


def _encode_range_bound(value: Any, elem_oid: int) -> bytes:
    """Binary-encode a stored range bound (bounds live in the element's storage
    form: int / Int64 / Decimal128 / datetime)."""
    if elem_oid == 23:
        return struct.pack("!i", int(value))
    if elem_oid == 20:
        return struct.pack("!q", int(value))
    if elem_oid == 1700:
        return _encode_numeric(value.to_decimal() if isinstance(value, bson.Decimal128) else value)
    if elem_oid == 1082:
        return _encode_date(value)
    return _encode_timestamptz(value)  # 1114 / 1184


def _encode_range(value: Any, range_oid: int) -> bytes:
    """Binary-encode a stored range subdocument (flags byte + bounds)."""
    elem_oid = _RANGE_ELEM_OID[range_oid]
    if not isinstance(value, dict) or value.get("empty"):
        return struct.pack("!B", _RANGE_EMPTY)
    flags = 0
    if value.get("lower_inc"):
        flags |= _RANGE_LB_INC
    if value.get("upper_inc"):
        flags |= _RANGE_UB_INC
    lo, hi = value.get("lower"), value.get("upper")
    if lo is None:
        flags |= _RANGE_LB_INF
    if hi is None:
        flags |= _RANGE_UB_INF
    out = bytearray(struct.pack("!B", flags))
    for bound in (lo, hi):
        if bound is None:
            continue
        b = _encode_range_bound(bound, elem_oid)
        out += struct.pack("!i", len(b)) + b
    return bytes(out)


def _encode_multirange(value: Any, mr_oid: int) -> bytes:
    range_oid = _MULTIRANGE_RANGE_OID[mr_oid]
    members = value.get("multirange", []) if isinstance(value, dict) else []
    out = bytearray(struct.pack("!i", len(members)))
    for m in members:
        b = _encode_range(m, range_oid)
        out += struct.pack("!i", len(b)) + b
    return bytes(out)


def _encode_numeric(value: Any) -> bytes:
    """Encode a Decimal as Postgres' binary ``numeric`` (base-10000 digits)."""
    d = value if isinstance(value, Decimal) else Decimal(str(value))
    if d.is_nan():
        return struct.pack("!HhHH", 0, 0, 0xC000, 0)
    if d.is_infinite():
        return struct.pack("!HhHH", 0, 0, 0xF000 if d < 0 else 0xD000, 0)
    sign = 0x4000 if d < 0 else 0x0000
    # copy_abs is context-free — unary minus would round to the (28-digit)
    # context precision and corrupt long numerics.
    d = d.copy_abs()
    _, digits_t, exp_t = d.as_tuple()
    dscale = -exp_t if exp_t < 0 else 0
    if exp_t >= 0:
        intg, frac = "".join(map(str, digits_t)) + "0" * exp_t, ""
    else:
        ds = "".join(map(str, digits_t))
        if len(ds) <= -exp_t:
            ds = "0" * (-exp_t - len(ds) + 1) + ds
        intg, frac = ds[:exp_t], ds[exp_t:]
    intg = intg or "0"
    intg = "0" * ((-len(intg)) % 4) + intg
    frac = frac + "0" * ((-len(frac)) % 4)
    groups_int = [int(intg[i : i + 4]) for i in range(0, len(intg), 4)]
    digits = groups_int + [int(frac[i : i + 4]) for i in range(0, len(frac), 4)]
    weight = len(groups_int) - 1
    while len(digits) > 1 and digits[0] == 0:  # strip leading zero groups
        digits.pop(0)
        weight -= 1
    while digits and digits[-1] == 0:  # strip trailing zero groups
        digits.pop()
    if not digits:
        return struct.pack("!HhHH", 0, 0, sign, dscale)
    out = bytearray(struct.pack("!HhHH", len(digits), weight, sign, dscale))
    for g in digits:
        out += struct.pack("!H", g)
    return bytes(out)


# Binary *output* encoders by Postgres type OID — the inverse of ``_BINARY``. The
# value arrives in its *storage* form (canonical text for date/time/net/uuid,
# subdocuments for interval/range/multirange). Text/varchar/json binary is just
# the UTF-8 text bytes; jsonb (3802) prefixes a version byte.
def _geo_nums(value: Any) -> list[float]:
    """Every float in a geometric value's canonical text spelling, in order."""
    import re as _re

    return [float(x) for x in _re.findall(r"-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?", str(value))]


def _encode_geo_floats(n: int) -> Any:
    def enc(value: Any) -> bytes:
        nums = _geo_nums(value)
        if len(nums) != n:
            raise ValueError(f"geometric value {value!r} does not have {n} numbers")
        return struct.pack(f"!{n}d", *nums)

    return enc


def _encode_geo_path(value: Any) -> bytes:
    text = str(value)
    closed = not text.startswith("[")
    nums = _geo_nums(value)
    pts = len(nums) // 2
    return struct.pack("!Bi", 1 if closed else 0, pts) + struct.pack(f"!{len(nums)}d", *nums)


def _encode_geo_polygon(value: Any) -> bytes:
    nums = _geo_nums(value)
    pts = len(nums) // 2
    return struct.pack("!i", pts) + struct.pack(f"!{len(nums)}d", *nums)


_OUT_BINARY = {
    16: _encode_bool,  # bool
    # Geometric results (Postgres' ``*_send`` layouts — the mirror of the
    # ``_GEO_BINARY`` parameter decoders above). pgjdbc's binary-mode
    # getObject constructs PGpoint/PGbox/... from these.
    600: _encode_geo_floats(2),  # point
    601: _encode_geo_floats(4),  # lseg
    602: _encode_geo_path,  # path
    603: _encode_geo_floats(4),  # box
    604: _encode_geo_polygon,  # polygon
    628: _encode_geo_floats(3),  # line
    718: _encode_geo_floats(3),  # circle
    17: lambda v: bytes(v),  # bytea
    18: lambda v: str(v).encode("utf-8"),  # "char" — raw byte(s), \0 included
    22: lambda v: _encode_int2vector(v),  # int2vector — an int2[] wire array
    4072: lambda v: b"\x01" + str(v).encode("utf-8"),  # jsonpath: version + text
    90010: lambda v: b"\x01" + str(v).encode("utf-8"),  # ltree: version + text
    90008: lambda v: str(v).encode("utf-8"),  # citext — text bytes
    20: lambda v: struct.pack("!q", int(typemap.unwrap_numeric(v))),  # int8
    21: lambda v: struct.pack("!h", int(typemap.unwrap_numeric(v))),  # int2
    23: lambda v: struct.pack("!i", int(typemap.unwrap_numeric(v))),  # int4
    26: lambda v: struct.pack("!I", int(typemap.unwrap_numeric(v)) & 0xFFFFFFFF),  # oid
    650: _encode_inet_factory(True),  # cidr
    700: lambda v: struct.pack("!f", float(typemap.unwrap_numeric(v))),  # float4
    701: lambda v: struct.pack("!d", float(typemap.unwrap_numeric(v))),  # float8
    829: _encode_macaddr,  # macaddr
    869: _encode_inet_factory(False),  # inet
    1082: _encode_date,  # date
    1083: _encode_time,  # time
    1114: _encode_timestamptz,  # timestamp (no tz)
    1184: _encode_timestamptz,  # timestamptz
    1186: _encode_interval,  # interval
    1266: _encode_timetz,  # timetz
    1700: lambda v: _encode_numeric(
        v.to_decimal() if isinstance(v, bson.Decimal128) else v
    ),  # numeric
    2950: _encode_uuid,  # uuid
}
_OUT_BINARY.update({oid: (lambda v, _o=oid: _encode_range(v, _o)) for oid in _RANGE_ELEM_OID})
_OUT_BINARY.update(
    {oid: (lambda v, _o=oid: _encode_multirange(v, _o)) for oid in _MULTIRANGE_RANGE_OID}
)


# Array type OID -> (element OID, element tag), derived from the same table the
# wire layer's RowDescription uses.
_ARRAY_ELEM_BY_OID: dict[int, tuple[int, str]] = {
    arr_oid: (typemap.PG_OID[elem], elem)
    for elem, arr_oid in typemap._ARRAY_PG_OID.items()
    if elem in typemap.PG_OID
}
# json[] (199 → json 114) — our ``json`` tag maps to jsonb (3802/3807), but a
# client can still bind an array parameter with the plain-json OIDs.
_ARRAY_ELEM_BY_OID[199] = (114, "json")
# varchar[]/bpchar[] — no internal tag of their own (values are text), but
# result columns report the real array oids (1015/1014) now that varchar/
# bpchar keep their type identity, so the binary encoder must know them.
_ARRAY_ELEM_BY_OID[1015] = (1043, "text")
_ARRAY_ELEM_BY_OID[1014] = (1042, "text")

# A user type's paired array oid is its own oid + USER_TYPE_ARRAY_OID_OFFSET
# (see catalog.py); everything at or above this floor is a user-type array whose
# elements travel as text (an enum's wire form is its label).
_USER_ARRAY_OID_FLOOR = ENUM_TYPE_OID_BASE + USER_TYPE_ARRAY_OID_OFFSET


def _array_elem_info(arr_oid: int) -> tuple[int, str]:
    """(element oid, element tag) for an array-type oid — modelled built-ins from
    the static table, user-type arrays derived from the offset scheme."""
    info = _ARRAY_ELEM_BY_OID.get(arr_oid)
    if info is not None:
        return info
    return (arr_oid - USER_TYPE_ARRAY_OID_OFFSET, "text")


def _array_dims(items: Any, elem_tag: str | None) -> list[int]:
    """The dimension lengths of a (possibly nested) array value. A list element
    is a sub-array only outside json[] (a JSON array IS a value there)."""
    dims: list[int] = []
    node = items
    while isinstance(node, (list, tuple)):
        dims.append(len(node))
        if (
            elem_tag != "json"
            and node
            and all(isinstance(v, (list, tuple)) for v in node)
            and len({len(v) for v in node}) == 1
        ):
            node = node[0]
        else:
            break
    return dims


def _encode_array(
    value: Any, arr_oid: int, tag: str | None, encoding: str | None = "utf-8"
) -> bytes:
    """Binary-encode an array result (ndim/hasnull/elemoid header, per-dim
    {len, lbound} pairs, then length-prefixed elements in row-major order).
    Nested lists encode as multi-dimensional arrays."""
    elem_oid, elem_tag = _array_elem_info(arr_oid)
    if isinstance(value, (list, tuple)):
        items = value
    else:
        items = typemap._parse_pg_array_literal(str(value))
    dims = _array_dims(items, elem_tag)
    flat: list[Any] = list(items)
    for _ in range(len(dims) - 1):
        flat = [v for sub in flat for v in (sub if isinstance(sub, (list, tuple)) else [sub])]
    has_null = any(v is None for v in flat)
    ndim = len(dims) if flat or len(dims) > 1 else 0
    out = bytearray(struct.pack("!iii", ndim, int(has_null), elem_oid))
    if ndim:
        for d in dims:
            out += struct.pack("!ii", d, 1)
        for v in flat:
            if v is None:
                out += struct.pack("!i", -1)
                continue
            if isinstance(v, str) and elem_tag not in ("text", "citext", "json"):
                # Elements parsed out of an array *text* literal are strings;
                # the element's binary encoder needs the native value
                # (``\x6162`` -> bytes, ``t`` -> bool, ``1`` -> int). A stored
                # ``json`` string element IS the value — don't re-parse it.
                if elem_tag == "bool":
                    v = v.strip().lower() in ("t", "true", "yes", "on", "1")
                else:
                    v = typemap.coerce(v, elem_tag)
            b = _encode_value(v, elem_oid, elem_tag, encoding)
            out += struct.pack("!i", len(b)) + b
    return bytes(out)


def _decode_array(
    raw: bytes, encoding: str | None = "utf-8", expected_elem_oid: int | None = None
) -> list:
    try:
        if expected_elem_oid is not None and len(raw) >= 12:
            embedded = struct.unpack_from("!i", raw, 8)[0]
            if (
                embedded
                and embedded != expected_elem_oid
                and (embedded in typemap.OID_TO_TAG or embedded in _BINARY)
            ):
                # The declared array type and the payload's embedded element
                # oid disagree on a KNOWN type (a jsonb[] payload bound as
                # json[]) — PG's 42804 datatype mismatch. A garbage/unknown
                # embedded oid falls through to the structural decode, whose
                # truncation surfaces as 08P01 — the pgtest corpus pins both.
                raise errors.SQLError(
                    "42804",
                    f"wrong element type: expected oid {expected_elem_oid}, got {embedded}",
                )
        return _decode_array_inner(raw, encoding)
    except (struct.error, IndexError) as e:
        # A truncated / structurally-bogus binary array parameter (the pgtest
        # corpus sends a bad element oid with no element data) is PG's 08P01,
        # never an internal error.
        raise errors.SQLError("08P01", "insufficient data left in message") from e


def _decode_array_inner(raw: bytes, encoding: str | None = "utf-8") -> list:
    """Decode a binary array parameter to a (possibly nested) Python list."""
    ndim, _has_null, elem_oid = struct.unpack_from("!iii", raw, 0)
    if ndim == 0:
        return []
    dims = []
    off = 12
    for _ in range(ndim):
        n, _lbound = struct.unpack_from("!ii", raw, off)
        dims.append(n)
        off += 8
    decoder = None if elem_oid in (0, 25, 1043) else _BINARY.get(elem_oid)
    total = 1
    for d in dims:
        total *= d
    items: list = []
    for _ in range(total):
        (length,) = struct.unpack_from("!i", raw, off)
        off += 4
        if length < 0:
            items.append(None)
            continue
        b = raw[off : off + length]
        off += length
        items.append(decoder(b) if decoder is not None else pgwire.decode_text(b, encoding))
    # Rebuild the nesting from the inside out.
    for d in reversed(dims[1:]):
        items = [items[i : i + d] for i in range(0, len(items), d)]
    return items


def _encode_value(
    value: Any, oid: int, tag: str | None, encoding: str | None = "utf-8"
) -> bytes | None:
    """Binary-encode a result value for ``oid``; None stays None (NULL on the wire).

    The binary form of text-shaped types is still text *in the client's
    encoding* — Postgres converts those like the text format."""
    if value is None:
        return None
    enc = _OUT_BINARY.get(oid)
    if enc is not None:
        return enc(value)
    if oid in _ARRAY_ELEM_BY_OID or oid >= _USER_ARRAY_OID_FLOOR:
        return _encode_array(value, oid, tag, encoding)
    if oid == 3802:  # jsonb: 1-byte version header + JSON text (client encoding)
        return b"\x01" + (pgwire.transcode_out(typemap.to_pg_text(value, tag), encoding) or b"")
    if (oid == 2249 or tag == "composite") and isinstance(value, dict):
        # Anonymous record (2249) or a declared composite under its minted oid.
        return _encode_record(value, encoding)
    if isinstance(value, dict) and "multirange" in value:
        # A user-declared multirange under its minted oid (built-ins matched
        # _OUT_BINARY above).
        return _encode_multirange_generic(value, encoding)
    if isinstance(value, dict) and ("empty" in value or ("lower" in value and "upper" in value)):
        return _encode_range_generic(value, encoding)
    if oid == 114:  # plain json: binary form is the bare JSON text (no header)
        return pgwire.transcode_out(typemap.to_pg_text(value, "json_plain"), encoding) or b""
    if (
        isinstance(value, dict)
        and tag not in ("json", "json_plain")
        and not any(k in value for k in ("hstore", "interval", "tsvector", "tsquery"))
    ):
        # A residual dict is a composite record (an array-of-composite element
        # reaches here with the element's minted oid) — ranges / multiranges /
        # jsonb and the tagged subdoc types were all matched above. JSON text
        # would blow up psycopg's binary record parser.
        return _encode_record(value, encoding)
    # text / varchar / unknown: the binary form equals the (client-encoded) text bytes.
    return pgwire.transcode_out(typemap.to_pg_text(value, tag), encoding) or b""


def _py_value_field_oid(v: Any) -> tuple[int, str]:
    """(oid, tag) for a record field's Python value — binary record fields carry
    their own per-field type oids."""
    if isinstance(v, bool):
        return (16, "bool")
    if isinstance(v, int):
        return (20, "int8")
    if isinstance(v, float):
        return (701, "float8")
    if isinstance(v, (Decimal, bson.Decimal128)):
        return (1700, "numeric")
    if isinstance(v, _dt.datetime):
        return (1184, "timestamptz") if v.tzinfo is not None else (1114, "timestamp")
    if isinstance(v, (bytes, bytearray, memoryview, bson.Binary)):
        return (17, "bytea")
    if isinstance(v, dict):
        return (2249, "composite")
    return (25, "text")


def _binary_record_to_text(
    raw: bytes, encoding: str | None = "utf-8", *, is_composite_oid: Any = None
) -> str:
    """Decode a PG binary record parameter into its record TEXT literal (fields
    decoded per their embedded oids), which rides the composite text parser.
    ``is_composite_oid`` (an ``oid -> bool`` predicate, when catalog context is
    available) lets a nested field embedded with a USER composite oid — not the
    generic 2249 — recurse instead of being text-decoded (its payload is raw
    binary and blows up UTF-8 decoding)."""
    (n,) = struct.unpack_from("!i", raw, 0)
    off = 4
    parts: list[str] = []
    for _ in range(n):
        oid, length = struct.unpack_from("!ii", raw, off)
        off += 8
        if length < 0:
            parts.append("")
            continue
        payload = bytes(raw[off : off + length])
        off += length
        decoder = None if oid in (0, 25, 1043) else _BINARY.get(oid)
        if decoder is not None:
            val = decoder(payload)
        elif oid == 2249 or (is_composite_oid is not None and is_composite_oid(oid)):
            val = _binary_record_to_text(payload, encoding, is_composite_oid=is_composite_oid)
        else:
            val = pgwire.decode_text(payload, encoding)
        rendered = typemap.to_pg_text(val, typemap.OID_TO_TAG.get(oid))
        text = rendered.decode("utf-8") if rendered is not None else str(val)
        if text == "" or any(ch in text for ch in ',()"\\') or any(ch.isspace() for ch in text):
            text = '"' + text.replace("\\", "\\\\").replace('"', '""') + '"'
        parts.append(text)
    return "(" + ",".join(parts) + ")"


def _oid_typname(oid: int) -> str:
    """The pg_type ``typname`` an error message names for ``oid`` (``16`` ->
    ``boolean``); ``-`` for an oid we don't know, matching PG's rendering."""
    tag = typemap.OID_TO_TAG.get(oid)
    return typemap.SQL_TYPE_NAME.get(tag, tag) if tag is not None else "-"


def _decode_binary_composite(raw: bytes, fields: list, encoding: str | None = "utf-8") -> str:
    """Decode a binary record parameter for a DECLARED composite type, validating
    it against the type's field list and raising PG's exact wire errors (the
    pgtest ``tuple`` corpus pins them via keepErrMessage). Returns the record
    TEXT literal so the value rides the existing composite text-cast path."""
    n_expected = len(fields)
    if len(raw) < 4:
        raise errors.SQLError("08P01", "insufficient data left in message")
    (nfields,) = struct.unpack_from("!i", raw, 0)
    off = 4
    if nfields != n_expected:
        raise errors.SQLError("42804", f"wrong number of columns: {nfields}, expected {n_expected}")
    parts: list[Any] = []
    for i, fld in enumerate(fields):
        expected_oid = typemap.PG_OID.get(fld[1], 0)
        if len(raw) - off < 4:
            raise errors.SQLError("08P01", "insufficient data left in message")
        (oid,) = struct.unpack_from("!i", raw, off)
        off += 4
        if oid != expected_oid:
            raise errors.SQLError(
                "42804",
                f"binary data has type {oid} ({_oid_typname(oid)}) instead of "
                f"expected {expected_oid} ({_oid_typname(expected_oid)}) in record column {i + 1}",
            )
        if len(raw) - off < 4:
            raise errors.SQLError("08P01", "insufficient data left in message")
        (length,) = struct.unpack_from("!i", raw, off)
        off += 4
        if length < 0:
            parts.append(None)  # NULL element
            continue
        if len(raw) - off < length:
            raise errors.SQLError("22P03", "insufficient data left in message")
        payload = bytes(raw[off : off + length])
        off += length
        # A fixed-width type recv that runs out of bytes (a 0-length bool) is PG's
        # 08P01 "no data left in message".
        if oid == 16 and length < 1:
            raise errors.SQLError("08P01", "no data left in message")
        decoder = _BINARY.get(oid)
        if decoder is not None:
            parts.append(decoder(payload))
        else:
            parts.append(pgwire.decode_text(payload, encoding))
    rendered: list[str] = []
    for val, fld in zip(parts, fields, strict=True):
        if val is None:
            rendered.append("")
            continue
        out = typemap.to_pg_text(val, fld[1])
        text = out.decode("utf-8") if out is not None else str(val)
        if text == "" or any(ch in text for ch in ',()"\\') or any(ch.isspace() for ch in text):
            text = '"' + text.replace("\\", "\\\\").replace('"', '""') + '"'
        rendered.append(text)
    return "(" + ",".join(rendered) + ")"


def _encode_record(value: dict, encoding: str | None = "utf-8") -> bytes:
    """PG binary record: int32 nfields, then per field int32 type oid +
    int32 length (-1 NULL) + data. A ``RecordValue`` carries its fields'
    declared SQL oids (``row('x')`` embeds unknown/705, ``row('x'::text)``
    25); otherwise the oid derives from the Python value."""
    declared = getattr(value, "field_oids", ())
    out = bytearray(struct.pack("!i", len(value)))
    for i, v in enumerate(value.values()):
        if v is None:
            # A NULL field still carries its declared type oid (a bare NULL in an
            # anonymous record is unknown/705); fall back to text when untyped.
            null_oid = declared[i] if i < len(declared) and declared[i] else 25
            out += struct.pack("!ii", null_oid, -1)
            continue
        oid, tag = _py_value_field_oid(v)
        if i < len(declared) and declared[i]:
            oid = declared[i]
            if oid == 705:
                # unknown — the value travels as its raw text bytes.
                b = pgwire.transcode_out(typemap.to_pg_text(v, "text"), encoding) or b""
                out += struct.pack("!ii", oid, len(b)) + b
                continue
            tag = _TAG_BY_OID.get(oid, tag)
        b = _encode_value(v, oid, tag, encoding) or b""
        out += struct.pack("!ii", oid, len(b)) + b
    return bytes(out)


def _encode_range_generic(rng: dict, encoding: str | None = "utf-8") -> bytes:
    """PG binary range for a user-declared range type — bounds encode in their
    Python value's natural binary form (the client's registered subtype loader
    expects exactly that)."""
    if rng.get("empty"):
        return bytes([_RANGE_EMPTY])
    flags = 0
    lo, hi = rng.get("lower"), rng.get("upper")
    if rng.get("lower_inc"):
        flags |= _RANGE_LB_INC
    if rng.get("upper_inc"):
        flags |= _RANGE_UB_INC
    if lo is None:
        flags |= _RANGE_LB_INF
    if hi is None:
        flags |= _RANGE_UB_INF
    out = bytearray([flags])
    for bound in (lo, hi):
        if bound is None:
            continue
        b_oid, b_tag = _py_value_field_oid(bound)
        b = _encode_value(bound, b_oid, b_tag, encoding) or b""
        out += struct.pack("!i", len(b)) + b
    return bytes(out)


def _encode_multirange_generic(mr: dict, encoding: str | None = "utf-8") -> bytes:
    rngs = mr.get("multirange", [])
    out = bytearray(struct.pack("!i", len(rngs)))
    for r in rngs:
        b = _encode_range_generic(r, encoding)
        out += struct.pack("!i", len(b)) + b
    return bytes(out)


def _result_value(
    value: Any, fmt: int, oid: int, tag: str | None, encoding: str | None = "utf-8"
) -> bytes | None:
    if fmt == 1:
        return _encode_value(value, oid, tag, encoding)
    if oid == 114 and tag == "json":  # plain json renders compact
        tag = "json_plain"
    return pgwire.transcode_out(typemap.to_pg_text(value, tag), encoding)


#: Escape hatch for the pipeline implicit-transaction feature (default ON —
#: PG semantics; pgjdbc's batch fidelity depends on it). The 2026-08-14
#: lost-update "conviction" that shipped it default-off was TIME-CONFOUNDED:
#: a fresh feature-ON CI round on healthy runners is green, and the losing
#: rounds all fell in the same degraded-runner window as the disk-reclaim
#: infra failures. The underlying degradation-triggered race in the SIMPLE
#: protocol path pre-exists on main and is tracked in tasks/backlog.md.
_PIPELINE_TXN_ENABLED = os.environ.get("SECANTUS_PIPELINE_TXN", "1") != "0"

#: Process-wide diagnostics for the lost-update hunt: the racing tests run
#: their server IN-PROCESS, so a failing assert can report whether the
#: implicit-txn machinery fired at all during the test (it should be zero
#: for pure simple-protocol traffic — a nonzero count falsifies the
#: protocol assumption on that platform).
COUNTERS = {"opened": 0, "settled": 0, "stmt_retry": 0, "settle_retry": 0, "joined": 0}


def _wants_implicit_txn(stmt: Any) -> bool:
    """Whether an extended-protocol statement should open the implicit
    transaction. Transaction-control statements manage blocks themselves, and
    statements PG refuses inside any transaction block (VACUUM …) must keep
    running bare — PG treats a lone pipelined statement as its own implicit
    transaction, which our per-statement autocommit already provides."""
    from sqlglot import exp as _exp

    if stmt is None:
        return False
    if isinstance(stmt, (_exp.Transaction, _exp.Commit, _exp.Rollback)):
        return False
    if isinstance(stmt, _exp.Command):
        head = str(stmt.this or "").strip().upper()
        return head not in (
            "BEGIN",
            "START",
            "COMMIT",
            "END",
            "ROLLBACK",
            "ABORT",
            "VACUUM",
            "SAVEPOINT",
            "RELEASE",
            "PREPARE",
            "DEALLOCATE",
            "DISCARD",
        )
    return True


def _column_formats(result_formats: list[int], ncols: int) -> list[int]:
    """Expand Bind's result-format codes to one per column."""
    if not result_formats:
        return [0] * ncols
    if len(result_formats) == 1:
        return [result_formats[0]] * ncols
    return [result_formats[i] if i < len(result_formats) else 0 for i in range(ncols)]


def _reject_nul(text: str) -> str:
    if "\x00" in text:
        raise errors.SQLError("22021", 'invalid byte sequence for encoding "UTF8": 0x00')
    return text


# Text-format parameter conversions by declared type OID. The declared type
# governs the parameter's value regardless of wire format (a text-format int8
# param IS an integer, exactly like its binary twin), so the common unambiguous
# scalars convert here — leaving the str through makes ``$1`` compare and encode
# as text while the RowDescription/Describe machinery reports the declared OID.
def _text_param_timestamp(tag: str):
    def conv(s: str) -> Any:
        return typemap.coerce(s, tag)

    return conv


def _text_param_interval(s: str) -> Any:
    from secantus.sql import intervals as _intervals

    return _intervals.parse(s)


def _text_param_date(s: str) -> Any:
    from secantus.sql import datetimes as _datetimes

    return typemap.DateText(_datetimes.parse_date(s))


def _text_param_time(s: str) -> Any:
    from secantus.sql import datetimes as _datetimes

    return typemap.TimeText(_datetimes.parse_time(s))


def _text_param_timetz(s: str) -> Any:
    from secantus.sql import datetimes as _datetimes

    return typemap.TimeTzText(_datetimes.parse_timetz(s))


def _text_param_bytea(s: str) -> bytes:
    from secantus.sql import bytea as _bytea

    return _bytea.parse(s)


def _text_param_uuid(s: str) -> str:
    from secantus.sql import uuidtype as _uuidtype

    return _uuidtype.normalize(s)


_TEXT_PARAM = {
    16: lambda s: s.strip().lower() in ("t", "true", "y", "yes", "on", "1"),  # bool
    17: _text_param_bytea,  # bytea — ``\x…`` hex (or escape) text form -> bytes
    20: int,  # int8
    21: int,  # int2
    23: int,  # int4
    26: int,  # oid
    700: float,  # float4
    701: float,  # float8
    1700: Decimal,  # numeric
    # Temporal params: the declared type governs the value regardless of wire
    # format — left as raw text, ``'lit'::timestamp = $1`` compares datetime vs
    # str and is silently false.
    1082: _text_param_date,  # date -> canonical text (dates are stored as text)
    1083: _text_param_time,  # time -> canonical text
    1114: _text_param_timestamp("timestamp"),  # -> naive datetime
    1184: _text_param_timestamp("timestamptz"),  # -> aware datetime
    1186: _text_param_interval,  # -> interval subdoc
    1266: _text_param_timetz,  # timetz -> canonical text
    2950: _text_param_uuid,  # uuid -> canonical hyphenated (psycopg dumps bare hex)
    # Network types -> the stored canonical form (a bare host inet gets /32).
    869: lambda s: typemap.coerce(s, "inet"),
    650: lambda s: typemap.coerce(s, "cidr"),
    829: lambda s: typemap.coerce(s, "macaddr"),
}


def _decode_param(raw: bytes | None, fmt: int, oid: int, encoding: str | None = "utf-8") -> Any:
    if raw is None:
        return None
    if oid == 2249:
        # The generic RECORD / anonymous composite type has no field types, so a
        # value can't be parsed into it — PG rejects at Bind (a declared
        # composite type carries a minted oid and decodes fine; only 2249 here).
        raise errors.SQLError("0A000", "input of anonymous composite types is not implemented")
    if fmt == 0:  # text
        text = _reject_nul(pgwire.decode_text(raw, encoding))
        if oid in (114, 3802):
            # A json/jsonb-declared text param — mark it so substitution casts
            # it into a parsed JSON value instead of leaving raw text.
            return typemap.JsonText(text)
        range_tag = _RANGEISH_TAG_BY_OID.get(oid)
        if range_tag is not None:
            # Range / multirange (or arrays of them): carried as text with the
            # declared tag so substitution casts it into the structured value.
            return typemap.TaggedText(text, range_tag)
        arr = _ARRAY_ELEM_BY_OID.get(oid)
        if arr is not None and arr[1] not in ("text", "citext", "json"):
            # A typed array param (int2[]/bytea[]/inet[]/…) — parse the array
            # literal into a typed list so the substitution's ``::tag[]`` cast
            # path fires (a raw text literal compares text-vs-list and is
            # silently false). text[] stays raw text: its literal form is
            # already what the text machinery expects.
            try:
                return typemap.TypedList(typemap.coerce(text, f"{arr[1]}[]"), arr[1])
            except (ValueError, TypeError):
                return text
        conv = _TEXT_PARAM.get(oid)
        if conv is None:
            return text
        try:
            return conv(text.strip())
        except (ValueError, ArithmeticError) as exc:
            name = typemap.SQL_TYPE_NAME.get(_TAG_BY_OID.get(oid, ""), "?")
            raise errors.SQLError(
                "22P02", f'invalid input syntax for type {name}: "{text}"'
            ) from exc
        return _reject_nul(pgwire.decode_text(raw, encoding))
    if oid == 0 and bytes(raw) == b"\x00\x00\x00\x00":
        # An *untyped* binary parameter (psycopg dumps an empty multirange with
        # no subtype info as oid 0 + a zero member count). Postgres resolves
        # unknown params from the target column; the empty-multirange text form
        # coerces correctly against any multirange column.
        return "{}"
    if oid in (0, 25, 1043):
        # Binary text is still text in the client's encoding. A binary payload
        # for an *untyped* (oid 0) parameter — e.g. ``$1::GEOMETRY`` for a type
        # we don't model, which PG would have resolved and decoded with the
        # type's recv function — may not be valid text; surface PG's faithful
        # 22P03 rather than leaking a UnicodeDecodeError as a generic XX000.
        try:
            return _reject_nul(pgwire.decode_text(raw, encoding))
        except UnicodeDecodeError as exc:
            raise errors.SQLError("22P03", "invalid binary representation") from exc
    decoder = _BINARY.get(oid)
    if decoder is not None:
        return decoder(raw)
    if ENUM_TYPE_OID_BASE <= oid < _USER_ARRAY_OID_FLOOR:
        # A minted user-type oid: keep the payload raw — Bind resolves it with
        # the catalog (an enum's binary form is its label text; a composite's
        # is the binary record layout).
        return bytes(raw)
    if oid in _ARRAY_ELEM_BY_OID or oid >= _USER_ARRAY_OID_FLOOR:
        # A user-type array oid (a registered enum's array dumper binds with the
        # minted array oid); the embedded element oid is unknown to _BINARY, so
        # elements fall back to text — which IS an enum's value form.
        expected = _ARRAY_ELEM_BY_OID.get(oid)
        items = _decode_array(raw, encoding, expected[0] if expected else None)
        arr = _ARRAY_ELEM_BY_OID.get(oid)
        if arr is not None and arr[1] not in ("text", "citext", "json"):
            # Typed binary arrays substitute through a ``::tag[]`` cast so
            # equality against ``array[…]`` values compares element-wise.
            return typemap.TypedList(items, arr[1])
        return items
    return raw.decode(encoding or "utf-8", "replace")


class ExtendedSession:
    def __init__(self, storage: Any, session: Session) -> None:
        self.storage = storage
        self.session = session
        # Statements executed inside the current implicit transaction, and
        # the last one's bound AST (the Sync-time commit retry re-runs it
        # when a single-statement pipeline loses a commit race).
        self._implicit_stmts = 0
        self._last_implicit_bound = None
        self.catalog = Catalog(storage)
        self.prepared: dict[str, Prepared] = {}
        self.portals: dict[str, Portal] = {}
        self.skip_until_sync = False
        # Expose the wire-prepared registry to the Session so the pg_cursors /
        # pg_prepared_statements virtual tables (whose builders only see the
        # Session) can list this connection's prepared statements.
        session.wire_prepared = self.prepared
        # Expose live portals too — DROP TABLE must refuse while an active
        # portal in this session still reads the table (PG's 55006; pgtest
        # multiple_active_portals).
        session.wire_portals = self.portals

    _TXN_EXIT_RE = None  # compiled lazily below

    def _targets_txn_exit(self, msg_type: str, payload: bytes) -> bool:
        """Whether this message names/contains a transaction-exit statement
        (COMMIT / ROLLBACK / ABORT / END) — permitted inside an aborted
        transaction, like PG's IsTransactionExitStmt."""
        import re as _re

        if ExtendedSession._TXN_EXIT_RE is None:
            ExtendedSession._TXN_EXIT_RE = _re.compile(r"^\s*(commit|rollback|abort|end)\b", _re.I)
        try:
            if msg_type == "P":
                _name, query, _oids = pgwire.parse_parse(payload, self.session.wire_encoding)
                return bool(ExtendedSession._TXN_EXIT_RE.match(query))
            if msg_type == "B":
                # Bind starts with two NUL-terminated names: portal, statement.
                parts = payload.split(b"\x00", 2)
                stmt_name = parts[1].decode("utf-8", "replace") if len(parts) > 1 else ""
                prep = self.prepared.get(stmt_name)
            elif msg_type == "E":
                portal_name, _max = pgwire.parse_execute(payload)
                portal = self.portals.get(portal_name)
                prep = portal.prepared if portal is not None else None
            else:  # "D"
                kind, name = pgwire.parse_describe(payload)
                if kind == "S":
                    prep = self.prepared.get(name)
                else:
                    portal = self.portals.get(name)
                    prep = portal.prepared if portal is not None else None
            stmt = getattr(prep, "stmt", None)
            if stmt is None:
                return False
            return isinstance(stmt, (exp.Commit, exp.Rollback)) or (
                isinstance(stmt, exp.Command)
                and bool(ExtendedSession._TXN_EXIT_RE.match(str(stmt.this or "")))
            )
        except Exception:
            # A malformed payload takes the normal path and surfaces its own
            # parse error rather than a misleading 25P02.
            return True

    def process(self, msg_type: str, payload: bytes) -> bytes:
        """Handle one extended-protocol message; return the bytes to send."""
        if msg_type == "S":  # Sync — always answered, clears any error state
            self.skip_until_sync = False
            out = bytearray()
            try:
                self._settle_implicit_txn()
            except errors.SQLError as exc:
                # A failed pipeline commit: the client must learn its
                # statements' effects are gone — ErrorResponse, then the
                # ReadyForQuery Sync always gets.
                out += pgwire.error_response(
                    exc.sqlstate,
                    exc.message,
                    encoding=self.session.wire_encoding,
                )
            # PG destroys portals at transaction end: once the implicit
            # cycle settles (and no explicit block remains open), suspended
            # portals are gone — a later Execute is 34000 (pgtest
            # multiple_active_portals). An open explicit block keeps its
            # portals alive across Sync.
            if self.session.txn_handle is None:
                self.portals.clear()
            # Settling the implicit transaction may have unwound SET LOCALs;
            # PG re-reports GUC_REPORT parameters at transaction end, so the
            # client's ParameterStatus cache reverts too (pgjdbc's
            # transactionalParameters* trio reads it right after Sync).
            for pname, pvalue in self.session.pending_parameter_status:
                out += pgwire.parameter_status(pname, pvalue)
            self.session.pending_parameter_status = []
            # Sync ends the message batch — the statement_timeout clock resets.
            self.session.clear_statement_deadline()
            return bytes(out) + pgwire.ready_for_query(self.session.txn_status())
        if self.skip_until_sync:
            return b""  # discard everything until the next Sync
        if msg_type == "H":  # Flush — we send eagerly, nothing to flush
            return b""
        if (
            msg_type in ("P", "B", "D", "E")
            and self.session.txn_failed
            and self.session.txn_handle is not None
            and not self.session.txn_is_implicit
            and not self._targets_txn_exit(msg_type, payload)
        ):
            # An ABORTED explicit transaction rejects every extended-protocol
            # step with 25P02 until it ends — except the transaction-exit
            # statements themselves (COMMIT/ROLLBACK), exactly PG's
            # IsTransactionExitStmt carve-out (the pgtest corpus pins the
            # Parse-in-aborted-txn shape).
            self.skip_until_sync = True
            return pgwire.error_response(
                "25P02",
                "current transaction is aborted, commands ignored until end of transaction block",
                encoding=self.session.wire_encoding,
            )
        try:
            # Parse/Bind/Describe read the catalog (pg_typeof rewrites, minted
            # user-type oid resolution, RowDescription) — inside an open
            # transaction block they must see the block's UNCOMMITTED DDL (a
            # type created two statements ago), so they run in the same storage
            # transaction Execute does.
            with self._txn_read_scope():
                if msg_type == "P":
                    return self._parse(payload)
                if msg_type == "B":
                    return self._bind(payload)
                if msg_type == "D":
                    return self._describe(payload)
            if msg_type == "E":
                # Arm statement_timeout for the batch on the first Execute (kept
                # until Sync, so a slow statement later in the batch still trips).
                self.session.arm_statement_deadline()
                return self._execute(payload)
            if msg_type == "C":
                return self._close(payload)
            self.skip_until_sync = True
            return pgwire.error_response("08P01", f"unexpected message type '{msg_type}'")
        except errors.SQLError as exc:
            self._poison_transaction()
            self.skip_until_sync = True
            return pgwire.error_response(
                exc.sqlstate,
                exc.message,
                encoding=self.session.wire_encoding,
                diag=getattr(exc, "diag", None),
                position=getattr(exc, "position", None),
            )
        except Exception:  # pragma: no cover - defensive
            logger.exception("error in extended protocol")
            self._poison_transaction()
            self.skip_until_sync = True
            # Generic wire message; full detail stays in the server log — don't
            # leak the raw Python exception text to the client. (§I17)
            return pgwire.error_response("XX000", "internal error")

    def _txn_read_scope(self) -> Any:
        """The open transaction's storage scope (or a no-op outside a block)."""
        import contextlib

        if self.session.txn_handle is not None and not self.session.txn_failed:
            return self.storage.use_user_transaction(self.session.txn_handle)
        return contextlib.nullcontext()

    # -- handlers ----------------------------------------------------------- #

    def _poison_transaction(self) -> None:
        """Mark an open transaction aborted after an error, as Postgres does.

        Any error inside a transaction block aborts it: every later statement
        fails with 25P02 until ROLLBACK. The engine already does this for
        errors raised while running a statement, but an error raised HERE — a
        missing prepared statement or portal, a Bind that cannot decode a
        parameter, a Describe on a bad name — never reached that path, so the
        block carried on as if nothing had happened. pgjdbc leans on it: after
        DEALLOCATE ALL invalidates its cached statements, the failed re-execute
        is expected to kill the transaction.
        """
        if self.session.txn_handle is not None:
            self.session.txn_failed = True

    def _parse(self, payload: bytes) -> bytes:
        name, query, oids = pgwire.parse_parse(payload, self.session.wire_encoding)
        if self.session.get_setting("standard_conforming_strings").lower() in ("off", "false", "0"):
            query = planner.decode_nonstandard_strings(query)
        stmts = planner.parse(query)
        if len(stmts) > 1:
            raise errors.syntax_error("cannot insert multiple commands into a prepared statement")
        stmt = stmts[0] if stmts else None
        if stmt is not None and engine.is_nonstatement_expression(stmt):
            # Garbage input ("SYNTAX ERROR") parses as a bare expression;
            # real PG rejects it AT PARSE TIME — pgx's Prepare and pipelined
            # SendPrepare both expect the ErrorResponse here, not at Execute.
            near = stmt.sql(dialect="postgres").split(None, 1)[0]
            raise errors.syntax_error(f'syntax error at or near "{near[:40]}"')
        count = planner.parameter_count(stmt) if stmt is not None else 0
        # Checked on the RAW statement, before the pg_typeof rewrite below
        # folds parameters out of the AST (that looked like a gap).
        gap = planner.parameter_numbering_gap(stmt)
        if gap is not None:
            raise errors.SQLError("42P18", f"could not determine data type of parameter ${gap}")
        if isinstance(stmt, exp.Select):
            # pg_typeof($N) types from the OIDs the client declares here in
            # Parse — after Bind substitutes values that information is gone.
            from secantus.sql import virtual

            planner.rewrite_pg_typeof(
                stmt,
                engine._pg_typeof_table(
                    self.storage, self.session.database, self.catalog, stmt.find(exp.Table)
                ),
                oids,
                user_type_name=lambda oid: virtual.user_type_name(
                    self.session.database, self.catalog, oid
                ),
            )
        # Parse-analysis type inference: a parameter the client left untyped
        # (oid 0) takes its type from the AST context (a cast on it, or a cast
        # operand it's compared with) so Bind can decode a BINARY payload.
        oids = planner.infer_parameter_types(
            stmt, list(oids), catalog=self.catalog, db=self.session.database
        )
        # An untyped parameter fed straight to a VARIADIC "any" function can't
        # be typed at all — PG rejects the Parse with 42P18.
        bad = planner.indeterminate_parameter(stmt, oids)
        if bad is not None:
            raise errors.SQLError("42P18", f"could not determine data type of parameter ${bad}")
        # One type per parameter: a type pinned by one use can make another use
        # unresolvable (``lower($1)`` with ``$1::int``) — PG's 42883.
        clash = planner.conflicting_parameter_use(stmt, oids)
        if clash is not None:
            raise errors.SQLError("42883", f"function {clash[0]}({clash[1]}) does not exist")
        # Postgres resolves comparison operators during parse analysis, so a
        # parameter DECLARED as a type that has no operator against the column
        # it is compared with (``varchar_col = $1`` with ``$1`` uuid) is a 42883
        # here, at Parse — not a predicate that matches nothing at Execute. The
        # execution path runs the same analysis without parameter types; only
        # Parse knows the declared OIDs. Sound-but-incomplete, and a no-op for
        # anything it cannot decide (see secantus.sql.typecheck).
        if stmt is not None:
            from secantus.sql import typecheck

            typecheck.check_statement(
                stmt, self.catalog, self.session.database, param_oids=list(oids)
            )
        if isinstance(stmt, exp.Copy):
            # PG's parse analysis gives COPY zero parameters — placeholders
            # inside the query survive to Execute, where an unbound one is
            # 42P02 (the pgtest copy corpus pins both error shapes).
            count = 0
            oids = []
        self.prepared[name] = Prepared(
            name, stmt, oids, count, query=query, created=_dt.datetime.now(_dt.timezone.utc)
        )
        return pgwire.parse_complete()

    def _bind(self, payload: bytes) -> bytes:
        portal, stmt_name, formats, raw_values, result_formats = pgwire.parse_bind(payload)
        prep = self.prepared.get(stmt_name)
        if prep is None:
            raise errors.SQLError("26000", f'prepared statement "{stmt_name}" does not exist')
        for code in (formats or []) + (result_formats or []):
            if code not in (0, 1):
                # PG validates format codes at Bind — 0 (text) and 1 (binary)
                # only (pgtest errors:95 sends ResultFormatCodes 2..5).
                raise errors.SQLError("08P01", f"unsupported format code: {code}")
        existing = self.portals.get(portal)
        if (
            portal
            and existing is not None
            and self.session.txn_handle is not None
            and not self.session.txn_is_implicit
            and existing.txn_token == id(self.session.txn_handle)
        ):
            summary = (prep.query or "").strip().rstrip(";")
            raise errors.SQLError(
                "42P03",
                f'portal "{portal}" already exists',
                diag={
                    "D": (
                        f'statement name "{stmt_name}"\n--\n'
                        f'portal name "{portal}"\n--\n'
                        f'statement summary "{summary}"'
                    )
                },
            )
        if raw_values and isinstance(prep.stmt, exp.Copy):
            summary = "COPY (SELECT) TO STDOUT" if not prep.stmt.args.get("kind") else "COPY"
            raise errors.SQLError(
                "08P01",
                f"bind message supplies {len(raw_values)} parameters, but requires 0",
                diag={"D": f'statement summary "{summary}"'},
            )
        # PG requires Bind to supply exactly as many parameters as the prepared
        # statement has — the DECLARED oids count even when the query uses
        # fewer (pgtest prepare declares three for a one-placeholder query and
        # rejects a one-parameter Bind). Checked AFTER the COPY case above,
        # whose error carries PG's statement-summary Detail.
        expected = max(len(prep.param_oids), prep.param_count)
        if len(raw_values) != expected:
            raise errors.SQLError(
                "08P01",
                f"bind message supplies {len(raw_values)} parameters, but requires {expected}",
            )
        values: list[Any] = []
        for i, raw in enumerate(raw_values):
            if not formats:
                fmt = 0
            elif len(formats) == 1:
                fmt = formats[0]
            else:
                fmt = formats[i]
            oid = prep.param_oids[i] if i < len(prep.param_oids) else 0
            value = _decode_param(raw, fmt, oid, self.session.wire_encoding)
            if oid == 2278 and value is None:
                # NULL declared as void: pgjdbc's function-OUT placeholder.
                # ``substitute_parameters`` drops it from the call's argument
                # list, mirroring PG's void-argument accommodation.
                values.append(planner.VOID_BIND)
                continue
            values.append(self._check_enum_param(oid, value))
        token = (
            id(self.session.txn_handle)
            if self.session.txn_handle is not None and not self.session.txn_is_implicit
            else None
        )
        new_portal = Portal(portal, prep, values, result_formats=result_formats, txn_token=token)
        # PG revalidates a named statement's cached plan during BIND: a result
        # shape that changed under DDL raises 0A000 INSTEAD of BindComplete, so
        # no portal is created (pgtest prepared_stmt_invalidation compares the
        # reply without ignoring BindComplete; aborted_txn ignores it, so Bind
        # satisfies both). Revalidating here also keeps it ahead of any side
        # effect, which is what the data-modifying-CTE case needs.
        if prep.name and prep.plan_shape is not None:
            shape = None
            try:
                cols = self._describe_columns(self._bound(new_portal))
                shape = [(c.name, c.pg_oid) for c in cols] if cols else None
            except errors.SQLError:
                # A statement we can't describe here (a missing relation, say)
                # surfaces its own error at Execute — don't mask it with a
                # cached-plan complaint. Only a SHAPE we could read counts.
                shape = None
            if shape is not None and shape != prep.plan_shape:
                raise errors.SQLError(
                    "0A000",
                    "cached plan must not change result type",
                    diag={"R": "RevalidateCachedQuery"},
                )
        self.portals[portal] = new_portal
        self._maybe_snapshot_execute(self.portals[portal])
        return pgwire.bind_complete()

    def _maybe_snapshot_execute(self, portal: Portal) -> None:
        """PG portals capture their snapshot at Bind: a portal bound inside an
        explicit transaction block keeps returning bind-time rows even when
        later same-transaction statements (DDL, writes) change the data
        underneath (pgtest's bind_and_resolve renames the table mid-block and
        still reads the old relname through the held portal). Surrogate: run
        read-only SELECT portals eagerly at Bind and stream the materialized
        rows at Execute. Errors must NOT surface here — PG reports execution
        errors at Execute, after BindComplete — so a failed eager run leaves
        the portal lazy and restores the block's aborted flag for Execute to
        trip for real."""
        session = self.session
        if session.txn_handle is None or session.txn_is_implicit:
            return
        stmt = portal.prepared.stmt
        if not isinstance(stmt, (exp.Select, exp.SetOperation)):
            return
        if stmt.find(exp.Insert, exp.Update, exp.Delete) is not None:
            return  # data-modifying CTE: side effects belong to Execute
        try:
            bound = self._bound(portal)
        except errors.SQLError:
            return
        prior_failed = session.txn_failed
        session.arm_statement_deadline()  # the eager run respects statement_timeout too
        try:
            # Mirror _execute's cached-plan revalidation: a named statement
            # whose result shape changed must raise 0A000 at Execute, so a
            # shape mismatch stays lazy instead of running here.
            prep = portal.prepared
            if prep.name and prep.plan_shape is not None:
                pre_cols = self._describe_columns(bound)
                if pre_cols and [(c.name, c.pg_oid) for c in pre_cols] != prep.plan_shape:
                    return
            portal.result = engine.run_statement(
                self.storage, session.database, bound, session, self.catalog
            )
        except Exception:
            session.txn_failed = prior_failed
            portal.result = None
            return
        res_cols = getattr(portal.result, "columns", None)
        if prep.name and res_cols and prep.plan_shape is None:
            prep.plan_shape = [(c.name, c.pg_oid) for c in res_cols]
        portal.executed = True
        portal.offset = 0

    def _check_enum_param(self, oid: int, value: Any) -> Any:
        """Resolve a parameter declared with a minted user-type oid: an enum
        param is label-validated at Bind (22P02 for a label the type doesn't
        have); a composite param's record text is tagged so substitution casts
        it into the typed subdocument."""
        if value is None or not isinstance(oid, int) or oid < ENUM_TYPE_OID_BASE:
            return value
        from secantus.sql import scalar, virtual

        db = self.session.database
        name = virtual.user_type_name(db, self.catalog, oid)
        if name is None:
            return value
        enum = self.catalog.get_enum(db, name)
        if enum is not None:
            if isinstance(value, bytes):  # binary enum form IS the label bytes
                value = pgwire.decode_text(value, self.session.wire_encoding)
            return scalar.validate_enum_label(enum, value)
        composite_fields = self.catalog.get_composite(db, name)
        if composite_fields is not None:
            if isinstance(value, bytes):
                # Validate the binary record against the declared field list and
                # raise PG's exact wire errors (pgtest tuple corpus); a well-formed
                # payload becomes the record text for the composite text-cast path.
                value = _decode_binary_composite(
                    value, composite_fields, self.session.wire_encoding
                )
            return typemap.TaggedText(str(value), virtual.quote_type_name(name))
        rng = getattr(self.catalog, "get_range_type", None)
        rng_doc = rng(db, name) if rng is not None else None
        if rng_doc is not None:
            if isinstance(value, bytes):
                # Binary custom range: PG's range wire layout with the declared
                # subtype's bound encoding (multirange when the oid names the
                # companion type).
                elem_oid = typemap.PG_OID.get(rng_doc.get("subtype_tag", "text"), 25)
                if name == rng_doc.get("multirange"):
                    (count,) = struct.unpack_from("!i", value, 0)
                    off, parts = 4, []
                    for _ in range(count):
                        (length,) = struct.unpack_from("!i", value, off)
                        off += 4
                        parts.append(_decode_range(value[off : off + length], elem_oid))
                        off += length
                    value = "{" + ",".join(parts) + "}"
                else:
                    value = _decode_range(value, elem_oid)
            return typemap.TaggedText(str(value), virtual.quote_type_name(name))
        if isinstance(value, bytes):  # unknown user type: best-effort text
            return pgwire.decode_text(value, self.session.wire_encoding)
        return value

    def _settle_implicit_txn(self) -> None:
        """Commit (or roll back, if poisoned) the implicit transaction at
        Sync — the pipeline boundary. Explicit blocks (BEGIN took over) are
        untouched."""
        session = self.session
        if session.txn_handle is None or not session.txn_is_implicit:
            return
        COUNTERS["settled"] += 1
        while True:
            try:
                if session.txn_failed:
                    engine._rollback_txn(self.storage, session)
                else:
                    engine._commit_txn(self.storage, self.session.database, self.catalog, session)
                return
            except errors.SQLError as exc:
                # A commit-time serialization loss on a SINGLE-statement
                # pipeline (plain autocommit traffic) re-runs the statement in
                # a fresh implicit transaction — the already-sent
                # CommandComplete becomes true, exactly like the internal
                # retry the per-statement path used to do. Only write
                # statements can lose a commit race, so re-running never
                # changes rows a client already received.
                if (
                    exc.sqlstate == "40001"
                    and self._implicit_stmts == 1
                    and self._last_implicit_bound is not None
                ):
                    COUNTERS["settle_retry"] += 1
                    with contextlib.suppress(Exception):
                        engine._rollback_txn(self.storage, session)
                    session.txn_handle = self.storage.begin_user_transaction()
                    session.txn_failed = False
                    session.txn_is_implicit = True
                    engine.run_statement(
                        self.storage,
                        self.session.database,
                        self._last_implicit_bound,
                        session,
                        self.catalog,
                    )
                    continue
                # A multi-statement pipeline's effects are gone — the client
                # must see the ERROR (silently swallowing one lost a
                # committed-looking increment on the Windows lane).
                with contextlib.suppress(Exception):
                    engine._rollback_txn(self.storage, session)
                raise

    def _describe(self, payload: bytes) -> bytes:
        kind, name = pgwire.parse_describe(payload)
        if kind == "S":
            prep = self.prepared.get(name)
            if prep is None:
                raise errors.SQLError("26000", f'prepared statement "{name}" does not exist')
            n = max(prep.param_count, len(prep.param_oids))
            # Postgres' parse analysis resolves an undeclared (oid 0) parameter
            # to text when nothing else pins it, and REPORTS that resolution —
            # psycopg re-dumps its parameters per this reply, so echoing 0 back
            # leaves a binary unknown-type param undecodable server-side.
            oids = [
                prep.param_oids[i] if i < len(prep.param_oids) and prep.param_oids[i] else 25
                for i in range(n)
            ]
            out = bytearray(pgwire.parameter_description(oids))
            stmt = (
                planner.substitute_parameters(prep.stmt, [None] * prep.param_count)
                if prep.stmt is not None
                else None
            )
            cols = self._describe_columns(stmt)
            _apply_param_result_oids(cols, prep)
            out += self._row_desc_or_no_data(cols)
            return bytes(out)
        # Portal describe — params are bound, so describe the bound statement, and
        # report the per-column result formats the client asked for in Bind.
        portal = self.portals.get(name)
        if portal is None:
            # A DECLAREd server-side cursor IS a portal in the v3 protocol —
            # psycopg's ServerCursor sends Describe('P', name) right after the
            # DECLARE to learn the row shape.
            cursor = self.session.cursors.get(name)
            if cursor is not None:
                return self._row_desc_or_no_data(list(cursor.columns))
            raise errors.SQLError("34000", f'unknown portal "{name}"')
        cols = self._describe_columns(self._bound(portal))
        _apply_param_result_oids(cols, portal.prepared)
        formats = _column_formats(portal.result_formats, len(cols)) if cols else None
        return self._row_desc_or_no_data(cols, formats)

    def _execute_copy_out(self, portal: Portal) -> bytes:
        """``COPY (query) TO STDOUT`` through the extended protocol (pgtest's
        copy corpus): CopyOutResponse + one CopyData per row + CopyDone +
        CommandComplete, all in the Execute reply. COPY FROM (and binary TO)
        keep the simple-protocol-only rejection."""
        stmt = portal.prepared.stmt
        unbound = next(stmt.find_all(exp.Parameter), None)
        if unbound is not None:
            raise errors.SQLError("42P02", f"there is no parameter ${unbound.name}")
        plan = engine.copy_plan(
            stmt, self.storage, self.session.database, self.catalog, self.session
        )
        if not plan.to_stdout or plan.fmt == "binary":
            raise errors.SQLError(
                "42601", "COPY ... TO/FROM STDIN/STDOUT must be a standalone statement"
            )
        if self.session.txn_handle is not None:
            with self.storage.use_user_transaction(self.session.txn_handle):
                rows = engine.copy_extract(
                    self.storage, self.session.database, self.catalog, self.session, plan
                )
        else:
            rows = engine.copy_extract(
                self.storage, self.session.database, self.catalog, self.session, plan
            )
        out = bytearray(pgwire.copy_out_response(len(plan.columns)))
        chunks: list[str] = []
        if plan.fmt == "csv":
            if plan.header:
                chunks.append(
                    copyfmt.format_csv(
                        [],
                        delimiter=plan.delimiter,
                        null=plan.null,
                        header=plan.columns,
                        quote=plan.quote or '"',
                    )
                )
            chunks += [
                copyfmt.format_csv(
                    [row], delimiter=plan.delimiter, null=plan.null, quote=plan.quote or '"'
                )
                for row in rows
            ]
        else:
            chunks += [
                copyfmt.format_text([row], delimiter=plan.delimiter, null=plan.null) for row in rows
            ]
        for chunk in chunks:
            if chunk:
                out += pgwire.copy_data(pgwire.encode_text(chunk, self.session.wire_encoding))
        out += pgwire.copy_done()
        out += pgwire.command_complete(f"COPY {len(rows)}")
        return bytes(out)

    def _execute(self, payload: bytes) -> bytes:
        # A cancel that landed between statements is discarded, like real PG
        # (mirrors the simple protocol's clear in _handle_query).
        self.session.cancel_event.clear()
        portal_name, max_rows = pgwire.parse_execute(payload)
        portal = self.portals.get(portal_name)
        if portal is None:
            raise errors.SQLError("34000", f'unknown portal "{portal_name}"')
        if portal.prepared.stmt is None:
            return pgwire.empty_query_response()
        if isinstance(portal.prepared.stmt, exp.Copy):
            return self._execute_copy_out(portal)
        if not portal.executed:
            bound = self._bound(portal)
            # Statements pipelined before a Sync run in ONE implicit
            # transaction in PG — a later error rolls back the earlier
            # statements' effects (pgjdbc's batch semantics depend on it:
            # BatchFailureTest counts the surviving rows). Open it lazily on
            # the first Execute outside a block; Sync settles it.
            first_in_implicit = False
            if self.session.txn_handle is not None and self.session.txn_is_implicit:
                COUNTERS["joined"] += 1
            if (
                _PIPELINE_TXN_ENABLED
                and self.session.txn_handle is None
                and _wants_implicit_txn(bound)
            ):
                self.session.txn_handle = self.storage.begin_user_transaction()
                self.session.txn_failed = False
                self.session.txn_is_implicit = True
                self._implicit_stmts = 0
                first_in_implicit = True
                COUNTERS["opened"] += 1
            # PG's cached-plan revalidation happens at PLANNING time — before
            # any side effect. A data-modifying CTE (`WITH x AS (INSERT …)
            # SELECT *`) whose result shape changed must raise WITHOUT running
            # the INSERT (pgjdbc's AutoRollback WITH_INSERT_SELECT matrix
            # counts the rows). Shape via the read-only describe path.
            prep_ = portal.prepared
            if prep_.name and prep_.plan_shape is not None:
                pre_cols = self._describe_columns(bound)
                if pre_cols:
                    pre_shape = [(c.name, c.pg_oid) for c in pre_cols]
                    if pre_shape != prep_.plan_shape:
                        raise errors.SQLError(
                            "0A000",
                            "cached plan must not change result type",
                            diag={"R": "RevalidateCachedQuery"},
                        )
            # pg_stat_activity (#137): mark this backend active with its query for
            # the duration of execution; it stays as the last query when idle.
            # The ORIGINAL text ($1 placeholders intact), like real PG — the
            # bound render would inline parameter values, which both leaks
            # them into pg_stat_activity and makes a poll like pgx's
            # ``query like $1`` match its own row.
            sess = self.session
            sess.state = "active"
            sess.current_query = portal.prepared.query or (
                bound.sql(dialect="postgres") if bound is not None else ""
            )
            sess.query_start = _dt.datetime.now(_dt.timezone.utc)
            try:
                while True:
                    try:
                        portal.result = engine.run_statement(
                            self.storage, self.session.database, bound, self.session, self.catalog
                        )
                        break
                    except errors.SQLError as exc:
                        # The FIRST statement of an implicit transaction that
                        # loses a write-write race retries in a fresh implicit
                        # transaction — client-visible behavior identical to
                        # the per-statement autocommit path it replaced
                        # (PG's read-committed doesn't surface these either).
                        # Later pipeline statements can't retry (their
                        # predecessors would be silently re-run) and surface
                        # 40001 like PG's serialization failure.
                        if (
                            exc.sqlstate != "40001"
                            or not first_in_implicit
                            or not self.session.txn_is_implicit
                        ):
                            raise
                        COUNTERS["stmt_retry"] += 1
                        with contextlib.suppress(Exception):
                            engine._rollback_txn(self.storage, self.session)
                        self.session.txn_handle = self.storage.begin_user_transaction()
                        self.session.txn_failed = False
                        self.session.txn_is_implicit = True
            finally:
                sess.state = "idle"
            if self.session.txn_is_implicit:
                self._implicit_stmts += 1
                self._last_implicit_bound = bound
            # First execution captures the plan identity (see
            # Prepared.plan_shape); later executions are checked BEFORE
            # running, above. Named statements only: PG re-plans unnamed
            # statements per Bind and never raises this for them.
            res_cols = getattr(portal.result, "columns", None)
            if prep_.name and res_cols and prep_.plan_shape is None:
                prep_.plan_shape = [(c.name, c.pg_oid) for c in res_cols]
            portal.executed = True
            portal.offset = 0
        res = portal.result
        out = bytearray()
        for notice in getattr(res, "notices", ()) or ():
            severity, message = notice[0], notice[1]
            out += pgwire.notice_response(
                message,
                severity=severity,
                sqlstate=(
                    notice[2]
                    if len(notice) > 2
                    else ("01000" if severity == "WARNING" else "00000")
                ),
                encoding=self.session.wire_encoding,
                file=notice[3] if len(notice) > 3 else None,
                routine=notice[4] if len(notice) > 4 else None,
            )
        status = list(res.parameter_status)
        if self.session.pending_parameter_status:
            status += self.session.pending_parameter_status
            self.session.pending_parameter_status = []
        if _is_row_returning(res):
            rows = res.rows
            cols = res.columns
            # Describe reported bare-``$N`` columns with the client's declared
            # Parse OIDs; the execution result carries the engine-inferred types
            # (a text-format int2 param plans as text, a binary int as int4).
            # Encoding MUST match the RowDescription the client already saw, or
            # a binary-format column carries bytes of a different type/format.
            _apply_param_result_oids(cols, portal.prepared)
            fmts = _column_formats(portal.result_formats, len(cols))
            end = len(rows) if max_rows <= 0 else min(len(rows), portal.offset + max_rows)
            for row in rows[portal.offset : end]:
                out += pgwire.data_row(
                    [
                        _result_value(
                            typemap.blank_pad(v, c.pg_oid, c.typmod),
                            f,
                            c.pg_oid,
                            c.type_tag,
                            self.session.wire_encoding,
                        )
                        for v, f, c in zip(row, fmts, cols, strict=False)
                    ]
                )
            delivered = end - portal.offset
            portal.offset = end
            tag = res.command_tag
            if tag.startswith("SELECT "):
                # A portal Execute's CommandComplete counts the rows THAT
                # Execute delivered, not the portal's total — the final,
                # drained Execute reports ``SELECT 0`` (pgtest portals).
                tag = f"SELECT {delivered}"
            if max_rows > 0 and delivered == max_rows:
                # PG cannot know the portal is exhausted until an Execute
                # fetches beyond the last row: delivering EXACTLY max_rows
                # always suspends, even when nothing remains, and the
                # CommandComplete comes from the next Execute (pgtest
                # portals). Fewer than max_rows means the end was reached.
                out += pgwire.portal_suspended()
            else:
                out += pgwire.command_complete(tag)
        else:
            out += pgwire.command_complete(res.command_tag)
        # PG reports GUC changes AFTER the command's completion message, like
        # the simple-query path (pgtest param_status pins the order; a wire
        # test caught this path lagging behind that fix).
        for pname, pvalue in status:
            out += pgwire.parameter_status(pname, pvalue)
        return bytes(out)

    def _close(self, payload: bytes) -> bytes:
        kind, name = pgwire.parse_close(payload)
        if kind == "S":
            self.prepared.pop(name, None)
        else:
            self.portals.pop(name, None)
            # Close('P') on a DECLAREd cursor's name destroys the cursor (a
            # DECLAREd cursor is a portal in the v3 protocol).
            self.session.cursors.pop(name, None)
        return pgwire.close_complete()

    # -- helpers ------------------------------------------------------------ #

    def _bound(self, portal: Portal) -> exp.Expression | None:
        if portal.bound_stmt is None and portal.prepared.stmt is not None:
            portal.bound_stmt = planner.substitute_parameters(portal.prepared.stmt, portal.params)
        return portal.bound_stmt

    def _describe_columns(self, stmt: exp.Expression | None) -> list | None:
        if stmt is None:
            return None
        return engine.describe_statement(
            self.storage, self.session.database, stmt, self.session, self.catalog
        )

    def _row_desc_or_no_data(self, cols: list | None, formats: list[int] | None = None) -> bytes:
        if cols is None:
            return pgwire.no_data()
        return pgwire.row_description(
            [(c.name, c.pg_oid, c.typmod, c.table_oid, c.attnum) for c in cols],
            formats,
            encoding=self.session.wire_encoding,
        )


_TAG_BY_OID = {oid: tag for tag, oid in typemap.PG_OID.items()}


def _apply_param_result_oids(cols: list | None, prep: Prepared) -> None:
    """Give a bare ``$N`` output column the client's declared parameter OID.

    ``SELECT $1`` must describe with the type the client sent in Parse (psycopg
    binds a Python int as int8, not int4); planning sees only the substituted
    Python value, which can't carry that distinction."""
    if not cols or prep.stmt is None or not any(prep.param_oids):
        return
    if not isinstance(prep.stmt, exp.Select):
        return
    for i, e in enumerate(prep.stmt.expressions):
        if i >= len(cols):
            break
        inner = e.this if isinstance(e, exp.Alias) else e
        while isinstance(inner, exp.Paren) and inner.this is not None:
            inner = inner.this
        if not isinstance(inner, exp.Parameter):
            continue
        try:
            idx = int(inner.name) - 1
        except (TypeError, ValueError):
            continue
        if 0 <= idx < len(prep.param_oids) and prep.param_oids[idx]:
            oid = prep.param_oids[idx]
            cols[i].pg_oid = oid
            cols[i].type_tag = _TAG_BY_OID.get(oid, cols[i].type_tag)


def _is_row_returning(res: Any) -> bool:
    return bool(res.columns) or res.command_tag.startswith("SELECT")
