"""BSON <-> Postgres type mapping (the load-bearing table).

Documents are stored as opaque BSON — same discipline as the Mongo side. This
module is the single place where a SQL column type meets a BSON value, in both
directions:

- ``type_tag_for_sql`` maps a parsed ``sqlglot`` ``DataType`` to a stable
  internal *type tag* (``int8``, ``text``, ``numeric``, ...).
- ``PG_OID`` gives the Postgres type OID for a tag (used by the wire layer's
  ``RowDescription`` in a later phase; carried now so the seam exists).
- ``coerce`` turns a Python literal (from the SQL text) into the BSON value to
  store, per the column's declared tag.
- ``to_py`` renders a stored BSON value back to a plain Python value for a
  result row.

Only the P0 scalar tags are wired up; nested ``json`` falls back to passthrough
and ``bytea`` to a hex string. Decimal128 is the exact fit for ``numeric``.
"""

from __future__ import annotations

import contextlib
import contextvars
import datetime as _dt
import decimal as _decimal
import json as _json
import math as _math
import re as _re
import struct as _struct
from decimal import Decimal
from typing import Any

import bson
from sqlglot import exp

# Internal type tags -> Postgres type OID. Stable across the codebase; the wire
# layer reads these for RowDescription.
PG_OID: dict[str, int] = {
    "bool": 16,
    "bytea": 17,
    # PG's internal one-byte "char" (quoted; pg_type.typname = char, oid 18).
    # Distinct from char(n)/bpchar: size 1, truncates to one character on
    # input, and an empty/zero-byte input stores NULL (pgtest char corpus).
    "char1": 18,
    "int8": 20,
    "int2": 21,
    "int4": 23,
    # oid: an unsigned int4-like object identifier (psycopg's Oid wrapper).
    "oid": 26,
    "text": 25,
    "float4": 700,
    "float8": 701,
    "timestamptz": 1184,
    "timestamp": 1114,  # naive "timestamp without time zone"
    # Date-only / time-only types (stored as canonical text).
    "date": 1082,
    "time": 1083,
    "timetz": 1266,
    "numeric": 1700,
    "json": 3802,
    # A composite (row) value renders as the ``(f1,f2,…)`` record text literal. We
    # report the generic RECORD pseudo-type OID rather than minting a per-type OID.
    "composite": 2249,
    # Range types render as the ``[lower,upper)`` text form.
    "int4range": 3904,
    "numrange": 3906,
    "tsrange": 3908,
    "tstzrange": 3910,
    "int8range": 3926,
    "daterange": 3912,
    # Multirange types render as the ``{[a,b),[c,d)}`` text form.
    "int4multirange": 4451,
    "nummultirange": 4532,
    "tsmultirange": 4533,
    "tstzmultirange": 4534,
    "datemultirange": 4535,
    "int8multirange": 4536,
    # oid — an unsigned 4-byte object identifier.
    # Full-text search types.
    "tsvector": 3614,
    "tsquery": 3615,
    # Network address types.
    "inet": 869,
    "cidr": 650,
    "macaddr": 829,
    # Bit-string types (rendered as a '0'/'1' string).
    "bit": 1560,
    "varbit": 1562,
    # Interval type (rendered as the Postgres interval text form).
    "interval": 1186,
    # UUID type (rendered as the canonical lower-case hyphenated string).
    "uuid": 2950,
    # Refcursor: a server-side cursor's name (plpgsql OPEN … FOR). The value is
    # the name text; the oid is what tells a driver to FETCH from it.
    "refcursor": 1790,
    # Money type (a Decimal rendered as ``$1,234.56``).
    "money": 790,
    # Geometric types (stored as canonical Postgres text).
    "point": 600,
    "lseg": 601,
    "path": 602,
    "box": 603,
    "polygon": 604,
    "line": 628,
    "circle": 718,
    # hstore (contrib extension). No fixed catalog OID — extension-assigned — so we
    # use a stable placeholder; drivers treat an unknown OID as text, which is what
    # the canonical hstore text form renders as. Deliberately NOT added to
    # PG_TYPENAME, so ``to_regtype('hstore')`` stays NULL and SQLAlchemy's psycopg
    # connect-time hstore probe is a no-op (it must not register a fictional type).
    "hstore": 16935,
    # citext (contrib): case-insensitive text, stored as plain text — the
    # case-folding is a query-planner behaviour, not a value shape. Like
    # hstore, the extension has no fixed catalog OID, so we use crdb's stable
    # placeholder 90008 (the pgtest citext corpus reads it byte-for-byte in
    # ParameterDescription and RowDescription; drivers treat the unknown OID
    # as text). A distinct OID also keeps the inverted OID→typename map
    # collision-free — ``format_type(25)`` still resolves to "text" (an
    # explicit ``citext: 25`` was the old reason this entry didn't exist).
    "citext": 90008,
    # SQL/JSON path expressions, stored as PG's canonical text form
    # (jsonpath.canonicalize). Real catalog oid.
    "jsonpath": 4072,
    # ltree (contrib): a dotted label path, stored as text. Like citext and
    # hstore, no fixed catalog oid — crdb's stable placeholder (pgtest ltree
    # corpus reads it in ParameterDescription and RowDescription).
    "ltree": 90010,
    # xml (a real built-in type, OID 142). Stored as its text; validated
    # well-formed on cast / coerce.
    "xml": 142,
    # System vector types: a space-separated list of ints in text format. Used by
    # pg_index.indkey/indclass/indoption so a libpq client's catalog reflection
    # (SQLAlchemy's _SpaceVector) sees "1 2", not a JSON/array decoding.
    "int2vector": 22,
    "oidvector": 30,
    # aclitem — loaded as raw text (psycopg has no loader; it only needs the
    # faithful oid pair to classify the array type).
    "aclitem": 1033,
    # name — the 63-byte identifier type (values travel as text).
    "name": 19,
    # void — the result type of a function called for its side effect only
    # (pg_sleep). typlen 4, value NULL on the wire (pgtest void corpus).
    "void": 2278,
}

# Full-text search type tags — stored as subdocuments, rendered as their PG text.
_FTS_TAGS = frozenset({"tsvector", "tsquery"})

# Network address type tags — stored as canonical text.
_NET_TAGS = frozenset({"inet", "cidr", "macaddr"})

# Bit-string type tags — stored as a canonical '0'/'1' string.
_BIT_TAGS = frozenset({"bit", "varbit"})

# Geometric type tags — stored as canonical Postgres text.
_GEO_TAGS = frozenset({"point", "lseg", "path", "box", "polygon", "line", "circle"})

# Type tags whose value is a list rendered as a Postgres ``int2vector`` /
# ``oidvector`` (space-separated, not array braces / JSON).
_VECTOR_TAGS = frozenset({"int2vector", "oidvector"})

# Range type tags — stored as a subdocument, rendered as ``[lower,upper)``.
_RANGE_TAGS = frozenset({"int4range", "int8range", "numrange", "tsrange", "tstzrange", "daterange"})

# Multirange type tags — stored as ``{"multirange": [range, …]}``, rendered as
# ``{[a,b),[c,d)}``.
_MULTIRANGE_TAGS = frozenset(
    {
        "int4multirange",
        "int8multirange",
        "nummultirange",
        "tsmultirange",
        "tstzmultirange",
        "datemultirange",
    }
)

# Internal type tag -> SQL type name (for information_schema.columns.data_type
# and any place a human-facing type spelling is needed).
SQL_TYPE_NAME: dict[str, str] = {
    "int2": "smallint",
    "int4": "integer",
    "int8": "bigint",
    "oid": "oid",
    "float4": "real",
    "float8": "double precision",
    "numeric": "numeric",
    "text": "text",
    "bool": "boolean",
    "timestamptz": "timestamp with time zone",
    "timestamp": "timestamp without time zone",
    "date": "date",
    "time": "time without time zone",
    "timetz": "time with time zone",
    "json": "jsonb",
    "bytea": "bytea",
    "composite": "record",
    "bit": "bit",
    "varbit": "bit varying",
    "interval": "interval",
    "uuid": "uuid",
    "money": "money",
    "hstore": "hstore",
    "citext": "citext",
    "aclitem": "aclitem",
    "name": "name",
    "xml": "xml",
    **{t: t for t in _RANGE_TAGS},
    **{t: t for t in _MULTIRANGE_TAGS},
    **{t: t for t in _FTS_TAGS},
    **{t: t for t in _NET_TAGS},
    **{t: t for t in _GEO_TAGS},
}

# Internal type tag -> Postgres pg_type.typname (for pg_catalog.pg_type rows).
PG_TYPENAME: dict[str, str] = {
    "int2": "int2",
    "int4": "int4",
    "int8": "int8",
    "oid": "oid",
    "float4": "float4",
    "float8": "float8",
    "numeric": "numeric",
    "text": "text",
    "bool": "bool",
    "timestamptz": "timestamptz",
    "timestamp": "timestamp",
    "date": "date",
    "time": "time",
    "timetz": "timetz",
    "json": "jsonb",
    "bytea": "bytea",
    "composite": "record",
    "bit": "bit",
    "varbit": "varbit",
    "interval": "interval",
    "uuid": "uuid",
    "money": "money",
    "xml": "xml",
    **{t: t for t in _RANGE_TAGS},
    **{t: t for t in _MULTIRANGE_TAGS},
    **{t: t for t in _FTS_TAGS},
    **{t: t for t in _NET_TAGS},
    **{t: t for t in _GEO_TAGS},
}

# SQL type spelling -> internal tag, for ``'name'::regtype`` normalization. Both
# the internal (``int2``) and pretty (``smallint``) spellings resolve, plus the
# common aliases Postgres accepts.
_REGTYPE_SPELLINGS: dict[str, str] = {
    **{name: tag for tag, name in PG_TYPENAME.items()},
    **{name: tag for tag, name in SQL_TYPE_NAME.items()},
    "int": "int4",
    "serial": "int4",
    "bigserial": "int8",
    "smallserial": "int2",
    "float": "float8",
    "decimal": "numeric",
    "varchar": "text",
    "character varying": "text",
    "char": "text",
    "character": "text",
    "bpchar": "text",
    "json": "json",
    "timestamptz": "timestamptz",
    "timetz": "timetz",
    # ``"oid"[]`` DDL can't survive sqlglot's OID keyword; ``planner.parse``
    # rewrites it to this quoted spelling, which resolves back here.
    "secantus_oid": "oid",
    "jsonpath": "jsonpath",
    "ltree": "ltree",
}


def builtin_tag_for_name(name: str) -> str | None:
    """The internal tag for a *quoted* built-in type spelling, or None.

    ``CREATE TABLE t (c "cidr")`` parses the quoted name as a user-defined
    type; psycopg's own test fixtures build DDL exactly this way
    (``sql.Identifier(info.name)``), so quoted built-in names must resolve
    before the enum/domain fallback."""
    raw = str(name).strip()
    if raw == '"char"' or raw == "'char'":
        # The QUOTED spelling names PG's internal one-byte type (oid 18);
        # the unquoted keyword spelling stays bpchar/text below.
        return "char1"
    key = " ".join(raw.strip('"').lower().split())
    if key.endswith("[]"):
        elem = builtin_tag_for_name(key[:-2])
        return f"{elem}[]" if elem is not None and f"{elem}[]" in PG_OID else None
    return _REGTYPE_SPELLINGS.get(key.split("(", 1)[0].strip())


def regtype_from_oid(oid: int) -> str | None:
    """The pretty spelling ``<oid>::regtype`` prints for a type OID (``21`` ->
    ``smallint``, ``1005`` -> ``smallint[]``), or None for an OID we don't model
    (the caller raises Postgres' 42704 undefined_object)."""
    tag = OID_TO_TAG.get(oid)
    if tag is None:
        return None
    if is_array_tag(tag):
        elem = array_element_tag(tag)
        return f"{SQL_TYPE_NAME.get(elem, elem)}[]"
    return SQL_TYPE_NAME.get(tag, tag)


def oid_for_regtype(name: str) -> int | None:
    """The type oid for a (possibly aliased) type name, or None when unknown —
    ``to_regtype('int4')`` -> 23, ``to_regtype('nope')`` -> NULL. Array
    spellings resolve to the paired array type's oid. A double-quoted
    identifier (psycopg's ``sql.Identifier`` spelling: ``'"text"'``) resolves
    like the bare name."""
    raw = str(name).strip()
    if len(raw) >= 2 and raw.startswith('"') and raw.endswith('"'):
        raw = raw[1:-1]
        if raw != raw.lower():
            return None  # quoted identifiers keep case; built-ins are lowercase
    text = " ".join(raw.strip().lower().split())
    if text.endswith("[]"):
        base = text[:-2].split("(", 1)[0].strip()
        tag = _REGTYPE_SPELLINGS.get(base)
        return _ARRAY_PG_OID.get(tag) if tag is not None else None
    base = text.split("(", 1)[0].strip()
    tag = _REGTYPE_SPELLINGS.get(base)
    return PG_OID.get(tag) if tag is not None else None


def normalize_regtype(name: str) -> str:
    """Render a type name the way ``'name'::regtype`` prints in Postgres —
    normalized to the canonical pretty spelling (``'int4'`` -> ``integer``).
    Unknown names pass through unchanged (we don't model every catalog type)."""
    text = " ".join(str(name).strip().lower().split())
    if text.endswith("[]"):
        elem = normalize_regtype(text[:-2])
        return f"{elem}[]"
    base = text.split("(", 1)[0].strip()
    tag = _REGTYPE_SPELLINGS.get(base)
    return SQL_TYPE_NAME.get(tag, tag) if tag is not None else text


# sqlglot DataType.Type -> our type tag. Several SQL spellings collapse onto one
# tag (varchar/char -> text; double/float -> float8) the way Postgres widens
# them for storage purposes here.
_DATATYPE_TAGS: dict[Any, str] = {
    exp.DataType.Type.BIGINT: "int8",
    exp.DataType.Type.INT: "int4",
    exp.DataType.Type.SMALLINT: "int2",
    exp.DataType.Type.TINYINT: "int2",
    exp.DataType.Type.FLOAT: "float4",
    exp.DataType.Type.DOUBLE: "float8",
    exp.DataType.Type.DECIMAL: "numeric",
    exp.DataType.Type.BIGDECIMAL: "numeric",
    exp.DataType.Type.TEXT: "text",
    exp.DataType.Type.VARCHAR: "text",
    exp.DataType.Type.CHAR: "text",
    exp.DataType.Type.NCHAR: "text",
    exp.DataType.Type.NVARCHAR: "text",
    exp.DataType.Type.BOOLEAN: "bool",
    exp.DataType.Type.DATE: "date",
    exp.DataType.Type.TIME: "time",
    # ``TIMESTAMP`` / ``DATETIME`` are the naive "without time zone" type; only the
    # explicit ``TIMESTAMPTZ`` (``TIMESTAMP WITH TIME ZONE``) is tz-aware.
    exp.DataType.Type.DATETIME: "timestamp",
    exp.DataType.Type.TIMESTAMP: "timestamp",
    exp.DataType.Type.TIMESTAMPTZ: "timestamptz",
    exp.DataType.Type.JSON: "json",
    exp.DataType.Type.JSONB: "json",
    exp.DataType.Type.BINARY: "bytea",
    exp.DataType.Type.VARBINARY: "bytea",
    exp.DataType.Type.BIT: "bit",
    exp.DataType.Type.INTERVAL: "interval",
    exp.DataType.Type.UUID: "uuid",
    exp.DataType.Type.MONEY: "money",
    exp.DataType.Type.POINT: "point",
    exp.DataType.Type.HSTORE: "hstore",
    exp.DataType.Type.XML: "xml",
}


# Element tag -> Postgres array type OID (pg_type of the ``_elem`` array type).
# The values are Postgres' own array-type OIDs so a driver's array decoder picks
# the right element parser for a column of ``<type>[]``.
_ARRAY_PG_OID: dict[str, int] = {
    "bool": 1000,
    "int8": 1016,
    "int2": 1005,
    "int4": 1007,
    "oid": 1028,
    "text": 1009,
    "float4": 1021,
    "float8": 1022,
    "numeric": 1231,
    "timestamptz": 1185,
    "timestamp": 1115,
    "bytea": 1001,
    # Date / time distinct types.
    "date": 1182,
    "time": 1183,
    "timetz": 1270,
    # jsonb (our ``json`` tag maps to jsonb).
    "json": 3807,
    # Network address types.
    "inet": 1041,
    "cidr": 651,
    "macaddr": 1040,
    # Bit-string types.
    "bit": 1561,
    "varbit": 1563,
    # Interval.
    "interval": 1187,
    # UUID.
    "uuid": 2951,
    # Money.
    "money": 791,
    # XML.
    "xml": 143,
    # Geometric types.
    "point": 1017,
    "lseg": 1018,
    "path": 1019,
    "box": 1020,
    "polygon": 1027,
    "line": 629,
    "circle": 719,
    # Full-text search types.
    "tsvector": 3643,
    "tsquery": 3645,
    # aclitem.
    "aclitem": 1034,
    # name.
    "name": 1003,
    # Range types.
    "int4range": 3905,
    "numrange": 3907,
    "tsrange": 3909,
    "tstzrange": 3911,
    "int8range": 3927,
    "daterange": 3913,
    # Multirange types.
    "int4multirange": 6150,
    "nummultirange": 6151,
    "tsmultirange": 6152,
    "tstzmultirange": 6153,
    "datemultirange": 6155,
    "int8multirange": 6157,
    # oid.
}

# Register the array tags (``text[]`` -> 1009, ...) in PG_OID so the wire layer's
# RowDescription reports the array type OID for an array column, not the element's.
PG_OID.update({f"{elem}[]": oid for elem, oid in _ARRAY_PG_OID.items()})

# OID -> tag inverse (built after the array registration so array OIDs resolve;
# PG_OID has no duplicate OIDs).
OID_TO_TAG: dict[int, str] = {oid: tag for tag, oid in PG_OID.items()}
# Plain-json oids alias the single ``json`` tag (we deliberately collapse
# json/jsonb): a parameter declared oid 114 / 199 types as json, not text.
OID_TO_TAG.setdefault(114, "json")
OID_TO_TAG.setdefault(199, "json[]")


# The connection whose results are currently being rendered — set per
# connection by the PG servers so ``to_pg_text`` can honour session GUCs
# (TimeZone) without threading a session through every call site.
_render_session: contextvars.ContextVar[Any] = contextvars.ContextVar(
    "secantus_sql_render_session", default=None
)


def set_render_session(session: Any) -> None:
    """Bind the connection's session to this thread's render context."""
    _render_session.set(session)


_MONTH_ABBR = ("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
_DOW_ABBR = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")
_DATE_TEXT_RE = _re.compile(r"^\d{4}-\d{2}-\d{2}$")


def render_datestyle() -> tuple[str, str]:
    """The active session's DateStyle GUC as ``(style, order)`` — e.g.
    ``("GERMAN", "DMY")``; ``("ISO", "YMD")`` when unbound."""
    session = _render_session.get()
    if session is None:
        return ("ISO", "YMD")
    ds = session.get_setting("DateStyle") or "ISO, MDY"
    parts = [p.strip().upper() for p in ds.split(",")]
    style = parts[0] if parts and parts[0] else "ISO"
    order = parts[1] if len(parts) > 1 and parts[1] else "MDY"
    if style == "GERMAN":
        order = "DMY"
    return (style, order)


def _render_date_style(y: int, mo: int, d: int, style: str, order: str) -> str:
    if style == "GERMAN":
        return f"{d:02d}.{mo:02d}.{y:04d}"
    if style == "SQL":
        return f"{d:02d}/{mo:02d}/{y:04d}" if order == "DMY" else f"{mo:02d}/{d:02d}/{y:04d}"
    if style == "POSTGRES":
        return f"{d:02d}-{mo:02d}-{y:04d}" if order == "DMY" else f"{mo:02d}-{d:02d}-{y:04d}"
    return f"{y:04d}-{mo:02d}-{d:02d}"


def _render_timestamp_style(value: _dt.datetime, style: str, order: str) -> str:
    """A naive timestamp's text under a non-ISO DateStyle — the exact shapes
    psycopg's TimestampLoader parses per style/order."""
    frac = f".{value.microsecond:06d}".rstrip("0") if value.microsecond else ""
    clock = f"{value.hour:02d}:{value.minute:02d}:{value.second:02d}{frac}"
    if style == "POSTGRES":
        dow = _DOW_ABBR[value.weekday()]
        mon = _MONTH_ABBR[value.month - 1]
        if order == "DMY":
            return f"{dow} {value.day:02d} {mon} {clock} {value.year:04d}"
        return f"{dow} {mon} {value.day:02d} {clock} {value.year:04d}"
    return f"{_render_date_style(value.year, value.month, value.day, style, order)} {clock}"


def render_tzinfo() -> _dt.tzinfo:
    """The active session's TimeZone GUC as a tzinfo (UTC when unbound)."""
    session = _render_session.get()
    if session is None:
        return _dt.timezone.utc
    from secantus.sql.datetimes import session_tzinfo

    return session_tzinfo(session)


class JsonText(str):
    """Raw JSON text from a parameter declared json/jsonb (oid 114/3802).

    The marker survives to ``planner._value_to_node``, which substitutes the
    parameter as a ``::jsonb`` cast so the value parses into a real JSON value
    (dict/list/scalar) and types as json downstream — a bare string literal
    would stay text and double-encode on output."""


class TaggedText(str):
    """Canonical text of a parameter whose declared OID maps to a structured
    tag (range / multirange, incl. their array forms) — substituted as a
    ``::tag`` cast so the existing cast coercion turns it into the real value
    (a range subdoc compares equal to another subdoc; raw text never does)."""

    tag: str

    def __new__(cls, text: str, tag: str) -> TaggedText:
        obj = super().__new__(cls, text)
        obj.tag = tag
        return obj


class TypedList(list):
    """A list parameter that knows its element tag — substituted through a
    ``::tag[]`` cast so it compares as a typed array, not raw literal text."""

    elem_tag: str = "text"

    def __init__(self, items: Any = (), elem_tag: str = "text") -> None:
        super().__init__(items)
        self.elem_tag = elem_tag


class RegClassValue(int):
    """A resolved ``regclass`` value: numerically the relation's pg_class oid
    (so ``c.oid = 'name'::regclass`` joins work), rendered as the relation
    name like real PG (``SELECT 'pub'::regclass`` prints ``pub``)."""

    relname: str

    def __new__(cls, oid: int, relname: str) -> RegClassValue:
        self = super().__new__(cls, oid)
        self.relname = relname
        return self

    def __str__(self) -> str:  # text render shows the name, not the oid
        return self.relname

    def __eq__(self, other: Any) -> bool:
        # ``pg_typeof(x) = 'name'::regtype``: pg_typeof rewrites to the pretty
        # type-name STRING at plan time, so a reg-value must compare equal to
        # its own name as well as to its oid (PG compares oids; the string leg
        # is our plan-time representation meeting the resolved cast).
        if isinstance(other, str):
            return other == self.relname
        return int(self) == other if isinstance(other, int) else NotImplemented

    __hash__ = int.__hash__


class RecordValue(dict):
    """An anonymous ``row(…)`` record value: an f1..fN dict that ALSO carries
    each field's SQL type oid (0 = derive from the Python value). The binary
    record encoder embeds per-field oids, and only the source expression can
    distinguish an untyped literal (705) from ``::text`` (25) or ``::bytea``."""

    field_oids: tuple[int, ...] = ()


class DateText(str):
    """Canonical date text from a parameter declared ``date`` (oid 1082) —
    substituted as a ``::date`` cast so expressions type as date (``$1 + 1`` is
    date arithmetic, and the result describes as date, not int)."""


class TimeText(str):
    """Canonical time text from a ``time`` (1083) parameter — ``::time`` cast."""


class TimeTzText(str):
    """Canonical timetz text from a ``timetz`` (1266) parameter — ``::timetz``."""


def to_decimal128(d: Decimal) -> bson.Decimal128:
    """A ``Decimal`` as BSON ``Decimal128``, rounding into range when it is
    wider than the 34 significant digits Decimal128 holds (the same fallback
    :func:`coerce` applies to a stored ``numeric``)."""
    try:
        return bson.Decimal128(d)
    except _decimal.DecimalException:
        from bson.decimal128 import create_decimal128_context

        with _decimal.localcontext(create_decimal128_context()) as ctx:
            return bson.Decimal128(ctx.create_decimal(d))


def number_literal(text: str) -> Any:
    """The value of a non-string numeric literal, as Postgres types it.

    A decimal literal (``1.5``, ``0.000000``) is ``numeric``: exact, and it
    keeps the scale it was written with. Reading those as a float lost both —
    ``0.1 + 0.2 = 0.3`` answered false, ``SELECT 0.000000`` came back as ``0``,
    and a value wider than a double silently dropped digits. Decimal128 is what
    a ``numeric`` COLUMN already stores (see :func:`coerce`), so literals and
    stored values share one representation.

    Exponent notation (``1e3``) is numeric too — ``pg_typeof(1e3)`` is
    ``numeric``, checked against PostgreSQL 14.13, not float8 as one might
    expect. An integer literal stays an ``int``.

    Lives here rather than in the planner because the scalar evaluator needs
    exactly the same mapping, and the two carried separate copies of it — which
    is why fixing only the planner's left arithmetic on floats.
    """
    if "." in text or "e" in text.lower():
        d = Decimal(text)
        exponent = d.as_tuple().exponent
        if isinstance(exponent, int) and exponent > 0 and d.adjusted() < 34:
            # ``1.5e2`` parses to Decimal('1.5E+2'); Postgres shows 150, so
            # expand the exponent to match. Guarded by ``adjusted()`` so a
            # literal like ``1e400`` — whose integer form is 401 digits, past
            # what Decimal128 can hold — keeps its compact form instead of
            # raising. The wire renderer expands either shape identically; this
            # only makes the embedded value match what Postgres reports.
            with _decimal.localcontext() as ctx:
                ctx.prec = 40
                d = d.quantize(Decimal(1))
        return to_decimal128(d)
    return int(text)


# PG numeric.c constants for select_div_scale (ported below).
_NUMERIC_MIN_SIG_DIGITS = 16
_NUMERIC_MAX_DISPLAY_SCALE = 1000


def _div_operand_stats(d: Decimal) -> tuple[int, int, int]:
    """The ``(base-10000 weight, first base-10000 digit, display scale)`` of a
    numeric operand — what PG's ``select_div_scale`` reads off its NumericVar
    (digits are stored base-10000 there, DEC_DIGITS=4)."""
    t = d.as_tuple()
    exponent = t.exponent if isinstance(t.exponent, int) else 0
    dscale = max(0, -exponent)
    if d.is_zero():
        return 0, 0, dscale
    msd_exp = d.adjusted()  # base-10 exponent of the most significant digit
    weight = msd_exp // 4  # floor → the base-10000 group index, incl. negatives
    first = int(abs(d).scaleb(-4 * weight))
    return weight, first, dscale


def numeric_div(left: Decimal, right: Decimal) -> Decimal:
    """PG ``numeric / numeric``: the quotient at the result scale real Postgres
    derives (``select_div_scale``, numeric.c — ported and probed against a live
    14.13: 20 cases, byte-identical renders).

    The scale rule: estimate the quotient's weight from the operands' leading
    base-10000 digits (assuming var1 < var2 when their first digits tie), give
    the result ``16`` significant digits past that weight, and floor/ceiling by
    the operands' display scales and Postgres' 0..1000 display-scale range.
    Rounding is half-away-from-zero, as numeric's round always is."""
    w1, f1, s1 = _div_operand_stats(left)
    w2, f2, s2 = _div_operand_stats(right)
    qweight = w1 - w2
    if f1 <= f2:
        qweight -= 1
    rscale = _NUMERIC_MIN_SIG_DIGITS - qweight * 4
    rscale = max(rscale, s1, s2, 0)
    rscale = min(rscale, _NUMERIC_MAX_DISPLAY_SCALE)
    with _decimal.localcontext() as ctx:
        # Enough working digits that the quantize below only ever rounds the
        # true quotient: the integer part is ~4*qweight digits, plus rscale
        # fractional digits, plus guard.
        ctx.prec = max(40, 4 * abs(qweight) + rscale + 12)
        q = left / right
        return q.quantize(Decimal(1).scaleb(-rscale), rounding=_decimal.ROUND_HALF_UP)


def unwrap_numeric(value: Any) -> Any:
    """A ``Decimal128`` as a plain ``Decimal``; anything else unchanged.

    Decimal128 implements no Python numeric protocol, so ``int()`` / ``float()``
    / arithmetic / comparison all reject it. Any code that takes a value which
    might be a ``numeric`` and does arithmetic on it needs this first.
    """
    return value.to_decimal() if isinstance(value, bson.Decimal128) else value


def negate(value: Any) -> Any:
    """Arithmetic negation that also handles BSON ``Decimal128`` (which has no
    Python operators of its own)."""
    if isinstance(value, bson.Decimal128):
        return bson.Decimal128(-value.to_decimal())
    return -value


def type_tag_for_sql(datatype: exp.DataType) -> str | None:
    """Map a parsed SQL ``DataType`` to an internal tag, or None if unknown. An
    ``ARRAY`` type becomes ``<elem>[]`` (e.g. ``int4[]``)."""
    if datatype.this == exp.DataType.Type.ARRAY:
        inner = datatype.args.get("expressions") or []
        elem = type_tag_for_sql(inner[0]) if inner else None
        if elem is None and inner:
            # A quoted built-in element spelling (``"cidr"[]``) parses as a
            # user-defined element type — resolve it like the bare-name case.
            elem = builtin_tag_for_name(inner[0].sql(dialect="postgres"))
        if elem is None:
            return None
        # ``int[][]`` is the same type as ``int[]`` in Postgres — the bracket
        # count is decorative; dimensionality lives in the value.
        return elem if is_array_tag(elem) else f"{elem}[]"
    tag = _DATATYPE_TAGS.get(datatype.this)
    if tag is not None:
        return tag
    # Range types — sqlglot's DataType.Type enum names vary across versions, so
    # match on the rendered type name (``int4range`` etc.).
    name = datatype.sql(dialect="postgres").lower().strip()
    if name in _RANGE_TAGS or name in _MULTIRANGE_TAGS or name in _FTS_TAGS or name in _NET_TAGS:
        return name
    # Bit strings carry an optional length (``bit(8)`` / ``varbit(16)``); match on
    # the bare name. ``bit varying`` is the spelled-out ``varbit``.
    base = name.split("(", 1)[0].strip()
    if base == "bit varying":
        return "varbit"
    if base == "refcursor":
        return "refcursor"
    if base in _BIT_TAGS:
        return base
    if base == "interval" or base.startswith("interval "):
        return "interval"
    # Date-only / time-only spellings (``timetz`` isn't always a dedicated enum;
    # ``time(6)`` carries a precision).
    if base in ("timetz", "time with time zone", "timez"):
        return "timetz"
    if base in ("time", "time without time zone"):
        return "time"
    if base == "date":
        return "date"
    if base in _GEO_TAGS:
        return base
    if base == "pg_char_1":
        # planner.parse rewrites a ::"char" cast to this sentinel (sqlglot
        # collapses the quoted spelling into plain CHAR, losing the identity).
        return "char1"
    if base == "citext":
        return "citext"
    if base == "jsonpath":
        return "jsonpath"
    if base == "ltree":
        return "ltree"
    if base == "aclitem":
        return "aclitem"
    if base == "name":
        return "name"
    # ``oid`` parses as an ``exp.ObjectIdentifier`` (whose ``.sql()`` is "OID"),
    # not a DataType enum member — match on the rendered name.
    if base == "oid":
        return "oid"
    return None


#: Cast targets whose PG identity carries a length/precision modifier:
#: DataType.Type name -> (oid, array_oid, modifier kind). ``char`` encodes
#: n+4, ``numeric`` packs ((p<<16)|(s&0x7FF))+4, ``bit`` stores n bare, and
#: ``time`` stores the precision bare — matching how psycopg decodes fmod.
_TYPMOD_KINDS: dict[str, tuple[int, int, str]] = {
    "VARCHAR": (1043, 1015, "char"),
    "NVARCHAR": (1043, 1015, "char"),
    "CHAR": (1042, 1014, "char"),
    "NCHAR": (1042, 1014, "char"),
    "BPCHAR": (1042, 1014, "char"),
    "DECIMAL": (1700, 1231, "numeric"),
    "BIT": (1560, 1561, "bit"),
    "TIME": (1083, 1183, "time"),
    "TIMETZ": (1266, 1270, "time"),
    "TIMESTAMP": (1114, 1115, "time"),
    "TIMESTAMPTZ": (1184, 1185, "time"),
    "INTERVAL": (1186, 1187, "time"),
}

#: Sentinel offset for negative numeric scales: sqlglot can't parse
#: ``numeric(2,-3)``, so the engine pre-rewrites ``-s`` to ``NEGSCALE + s``
#: before parsing and the typmod encoder undoes it (real scales cap at 1000,
#: so the ranges never overlap).
NEGSCALE_SENTINEL = 5000


def _typmod_param(node: exp.Expression) -> int | None:
    try:
        return int(node.name)
    except (TypeError, ValueError):
        return None


def cast_type_identity(datatype: exp.DataType) -> tuple[int, int] | None:
    """``(pg_oid, typmod)`` for a cast target that carries a PG type modifier
    (``varchar(42)`` → (1043, 46), ``numeric(10,3)`` → (1700, ((10<<16)|3)+4),
    ``time(2)`` → (1083, 2)) — or a bare ``varchar``/``bpchar``, whose identity
    alone differs from the ``text`` oid we'd otherwise report. An array target
    reports the element's typmod with the array oid, matching PG. None when
    the target isn't a modifier-bearing type."""
    if datatype.this == exp.DataType.Type.ARRAY:
        inner = datatype.args.get("expressions") or []
        if inner and isinstance(inner[0], exp.DataType) and inner[0].this == exp.DataType.Type.JSON:
            # ``::JSON[]`` keeps the plain-json ARRAY identity (199), like the
            # scalar ``::json`` → 114 below — the tag collapses json into
            # jsonb, but the wire oids (and the binary array's element oid)
            # differ, and the pgtest corpus reads them byte-for-byte.
            return (199, -1)
        if inner and isinstance(inner[0], exp.DataType):
            elem = cast_type_identity(inner[0])
            if elem is not None:
                _oid, typmod = elem
                kinds = _TYPMOD_KINDS.get(getattr(inner[0].this, "name", None))
                if kinds is None:  # varbit via USERDEFINED
                    kinds = _userdefined_typmod_kind(inner[0])
                if kinds is not None:
                    return (kinds[1], typmod)
        return None
    # ``datatype.this`` is a plain string for some spellings (``::oid`` parses
    # via ObjectIdentifier) — those never carry a modifier.
    # ``::json`` keeps the plain-json identity (114) — our ``json`` tag maps to
    # jsonb (3802), whose binary form carries a version byte plain json lacks;
    # a client's json loader would choke on the prefix (and vice versa).
    if getattr(datatype.this, "name", None) == "JSON":
        return (114, -1)
    entry = _TYPMOD_KINDS.get(getattr(datatype.this, "name", None))
    if entry is None:
        ud = _userdefined_typmod_kind(datatype)
        if ud is None:
            return None
        entry = ud
    oid, _array_oid, kind = entry
    params = [p for p in (datatype.args.get("expressions") or []) if _typmod_param(p) is not None]
    if not params:
        # Bare varchar/bpchar still need their distinct oid; the other kinds
        # already report the right oid elsewhere, so a bare form changes nothing.
        return (oid, -1) if kind == "char" else None
    n = _typmod_param(params[0]) or 0
    if kind == "char":
        return (oid, n + 4)
    if kind == "bit":
        return (oid, n)
    if kind == "time":
        return (oid, n)
    # numeric(p[,s]) — a missing scale is 0; undo the negative-scale sentinel.
    s = _typmod_param(params[1]) if len(params) > 1 else 0
    s = s if s is not None else 0
    if s > NEGSCALE_SENTINEL - 1001:
        s = -(s - NEGSCALE_SENTINEL)
    return (oid, ((n << 16) | (s & 0x7FF)) + 4)


def _userdefined_typmod_kind(datatype: exp.DataType) -> tuple[int, int, str] | None:
    """``varbit(n)`` parses as USERDEFINED — resolve it by rendered name."""
    if datatype.this != exp.DataType.Type.USERDEFINED:
        return None
    name = datatype.sql(dialect="postgres").lower().split("(", 1)[0].strip().strip('"')
    if name in ("varbit", "bit varying"):
        return (1562, 1563, "bit")
    return None


def is_array_tag(tag: str | None) -> bool:
    return isinstance(tag, str) and tag.endswith("[]")


def array_element_tag(tag: str) -> str:
    """The element tag of an array tag (``int4[]`` -> ``int4``)."""
    return tag[:-2]


def normalize_result_value(value: Any, tag: str | None) -> Any:
    """Make an embedded result value present the way the wire encoders render it.

    A stored ``timestamptz`` decodes **tz-naive** UTC from BSON, but Postgres
    presents it tz-aware — the wire text (``to_pg_text``) and binary
    (``_encode_timestamptz``) paths already tag naive timestamptz values UTC, so
    ``run_sql`` should hand back the same tz-aware instant rather than a naive
    datetime that silently mis-compares against a tz-aware literal (#141).

    Only ``timestamptz`` (and arrays of it, at any nesting depth) are touched;
    ``timestamp`` / ``date`` / ``time`` stay correctly tz-naive."""
    if isinstance(value, (list, tuple)) and is_array_tag(tag):
        elem = array_element_tag(tag)
        return [
            normalize_result_value(v, elem + "[]" if isinstance(v, (list, tuple)) else elem)
            for v in value
        ]
    if tag == "timestamptz" and isinstance(value, _dt.datetime) and value.tzinfo is None:
        return value.replace(tzinfo=_dt.timezone.utc)
    return value


# Postgres' whitespace set for array literals — NOT Python str.strip()'s:
# \x1c-\x1f (and NBSP/NEL) are isspace() to Python but data to Postgres.
_PG_ARRAY_WS = " \t\n\r\v\f"


def _parse_pg_array_literal(text: str) -> list:
    """Parse a Postgres array *string* literal into a (possibly nested) Python
    list, following PG's grammar: an optional ``[l:u]…=`` dimension-bounds
    prefix, nested ``{}`` sub-arrays, double-quoted elements with ``\\X``
    escapes, backslash escapes in bare elements, and a bare unquoted ``NULL``
    as None."""
    s = text.strip(_PG_ARRAY_WS)
    # ``[0:1]={a,b}`` — dimension bounds; the values follow the ``=``.
    if s.startswith("["):
        depth_end = s.find("=")
        if depth_end == -1:
            raise ValueError(f"malformed array literal: {text!r}")
        s = s[depth_end + 1 :].strip(_PG_ARRAY_WS)
    if not s.startswith("{"):
        raise ValueError(f"malformed array literal: {text!r}")
    value, i = _parse_pg_array_body(s, 0)
    if s[i:].strip(_PG_ARRAY_WS):
        raise ValueError(f"malformed array literal: {text!r}")
    return value


def _parse_pg_array_body(s: str, i: int) -> tuple[list, int]:
    """Parse one ``{…}`` (sub-)array starting at ``i`` (which must point at the
    opening brace); returns ``(elements, next_index)``."""
    assert s[i] == "{"
    i += 1
    n = len(s)
    out: list = []
    while True:
        while i < n and s[i] in _PG_ARRAY_WS:
            i += 1
        if i >= n:
            raise ValueError(f"malformed array literal: {s!r}")
        if s[i] == "}" and not out:
            return out, i + 1  # empty array
        if s[i] == "{":
            sub, i = _parse_pg_array_body(s, i)
            out.append(sub)
        elif s[i] == '"':  # quoted element (keeps commas / literal NULL)
            i += 1
            buf: list[str] = []
            while i < n and s[i] != '"':
                if s[i] == "\\" and i + 1 < n:
                    buf.append(s[i + 1])
                    i += 2
                else:
                    buf.append(s[i])
                    i += 1
            if i >= n:
                raise ValueError(f"malformed array literal: {s!r}")
            i += 1  # closing quote
            out.append("".join(buf))
        else:  # bare element up to the next separator / close, with \X escapes
            buf = []
            while i < n and s[i] not in ",}":
                if s[i] == "\\" and i + 1 < n:
                    buf.append(s[i + 1])
                    i += 2
                else:
                    buf.append(s[i])
                    i += 1
            token = "".join(buf).strip(_PG_ARRAY_WS)
            out.append(None if token.upper() == "NULL" else token)
        while i < n and s[i] in _PG_ARRAY_WS:
            i += 1
        if i >= n:
            raise ValueError(f"malformed array literal: {s!r}")
        if s[i] == "}":
            return out, i + 1
        if s[i] != ",":
            raise ValueError(f"malformed array literal: {s!r}")
        i += 1


def _coercion_error(tag: str, value: Any) -> Exception:
    """A 22P02 that is ALSO a ValueError: paths that soft-catch ValueError
    around ``coerce`` keep their fallbacks, while an uncaught failure reaches
    the wire as invalid_text_representation instead of an internal error."""
    from secantus.sql import errors as _sql_errors

    class _CoercionError(_sql_errors.SQLError, ValueError):
        pass

    return _CoercionError(
        "22P02", f'invalid input syntax for type {SQL_TYPE_NAME.get(tag, tag)}: "{value}"'
    )


def _int_or_22p02(value: Any, tag: str) -> int:
    try:
        return int(value)
    except (TypeError, ValueError) as e:
        raise _coercion_error(tag, value) from e


def _render_timestamp_iso(value: _dt.datetime) -> str:
    """ISO text with Postgres' offset spelling.

    Python renders a zero-minute offset as ``+00:00``; Postgres writes ``+00``,
    widening to ``-05:30`` or ``+05:45:12`` only when it must. Clients compare
    the rendered text — pgjdbc's TimezoneTest asserts ``12:00:00+00`` — so the
    trailing ``:00`` is not cosmetic.
    """
    text = value.isoformat(sep=" ")
    if value.tzinfo is None:
        return text
    head, sign, offset = text.rpartition("+") if "+" in text[10:] else text.rpartition("-")
    if not sign or ":" not in offset:
        return text
    parts = offset.split(":")
    while len(parts) > 1 and parts[-1] == "00":
        parts.pop()
    return f"{head}{sign}{':'.join(parts)}"


def _to_session_wall_clock(value: _dt.datetime) -> _dt.datetime:
    """A tz-aware instant as naive local wall clock in the session zone —
    Postgres' ``timestamptz`` -> ``timestamp`` conversion."""
    session = _render_session.get()
    if session is None:
        return value.replace(tzinfo=None)
    from secantus.sql.datetimes import session_tzinfo

    with contextlib.suppress(OverflowError, ValueError):
        return value.astimezone(session_tzinfo(session)).replace(tzinfo=None)
    return value.replace(tzinfo=None)


def _representable_instant(value: _dt.datetime) -> _dt.datetime:
    """An aware datetime that BSON can store, else the same value tz-naive.

    BSON normalises an aware datetime to UTC on the way out, and near
    ``datetime.min`` that crosses out of Python's range — ``0001-01-01
    00:00+05:00`` is year zero in UTC. Those extremes keep their wall clock
    instead, which is what they did before offsets were parsed at all.
    """
    if value.tzinfo is None:
        return value
    try:
        value.astimezone(_dt.timezone.utc)
    except (OverflowError, ValueError):
        return value.replace(tzinfo=None)
    return value


def _as_session_instant(value: _dt.datetime) -> _dt.datetime:
    """Resolve a ``timestamptz`` input to an absolute instant.

    A literal carrying no offset is LOCAL TIME IN THE SESSION ZONE, not UTC —
    ``'1950-02-07'`` under ``America/New_York`` is midnight in New York, which
    Postgres reports back as ``1950-02-07 00:00:00-05``. Reading it as UTC
    shifted the value by the zone's offset, so the same date came back a day
    early or late depending on which side of Greenwich the client sat.

    A value that already carries an offset is absolute and passes through.
    """
    if value.tzinfo is not None:
        return value
    session = _render_session.get()
    if session is None:
        return value  # no connection bound (embedded API): unchanged, i.e. UTC
    from secantus.sql.datetimes import session_tzinfo

    with contextlib.suppress(OverflowError, ValueError):
        return value.replace(tzinfo=session_tzinfo(session)).astimezone(_dt.timezone.utc)
    return value


#: An ltree label path: alphanumeric/underscore labels joined by dots.
_LTREE_RE = _re.compile(r"[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*")


def coerce(value: Any, tag: str) -> Any:
    """Coerce a Python literal to the BSON value stored for column ``tag``.

    ``None`` passes through as SQL NULL. Unknown tags pass the value through
    unchanged (the reflected-table path, a later phase, leans on this).
    """
    if value is None:
        return None
    if isinstance(value, bson.Decimal128) and tag != "numeric":
        # A decimal literal now arrives as Decimal128 (see number_literal), and
        # Decimal128 supports no Python conversions — ``float(Decimal128)``
        # raises. Unwrap once here so every tag below converts from a plain
        # Decimal, which int() / float() / str() all accept. ``numeric`` is
        # excluded because its own branch already handles both forms and would
        # otherwise lose the fast path.
        value = value.to_decimal()
    if is_array_tag(tag):
        elem = array_element_tag(tag)
        if elem == "box" and not isinstance(value, (list, tuple)):
            # box[] is the one built-in whose array delimiter is ``;`` (a box's
            # own text form contains commas).
            body = str(value).strip(_PG_ARRAY_WS)
            if not (body.startswith("{") and body.endswith("}")):
                raise ValueError(f"malformed array literal: {value!r}")
            return [
                coerce(t.strip(_PG_ARRAY_WS), "box")
                for t in body[1:-1].split(";")
                if t.strip(_PG_ARRAY_WS)
            ]
        items = value if isinstance(value, (list, tuple)) else _parse_pg_array_literal(str(value))
        # Multi-dimensional literals nest lists; coerce each leaf to the
        # element type, preserving the nesting.
        return [coerce(v, tag) if isinstance(v, (list, tuple)) else coerce(v, elem) for v in items]
    if tag == "any":
        # Schema-on-read (reflected tables): keep the literal's natural Python
        # type so it compares against the stored BSON value as-is.
        return value
    if tag in _RANGE_TAGS:
        # Already-built subdocument (from a range constructor) passes through; a
        # text literal (``'[1,10)'``) is parsed to the subdocument form.
        if isinstance(value, dict):
            return value
        from secantus.sql import ranges as _ranges

        elem, _discrete = _ranges.RANGE_TYPES[tag]
        return _ranges.parse_literal(str(value), tag, lambda tok: coerce(tok, elem))
    if tag in _MULTIRANGE_TAGS:
        if isinstance(value, dict):
            return value
        from secantus.sql import ranges as _ranges

        range_tag = _ranges.MULTIRANGE_TYPES[tag]
        elem, _discrete = _ranges.RANGE_TYPES[range_tag]
        return _ranges.parse_multirange(str(value), tag, lambda tok: coerce(tok, elem))
    if tag in _FTS_TAGS:
        if isinstance(value, dict):
            return value
        from secantus.sql import fts as _fts

        return _fts.parse_tsvector(str(value)) if tag == "tsvector" else _fts.to_tsquery(str(value))
    if tag in _NET_TAGS:
        from secantus.sql import net as _net

        if tag == "inet":
            return _net.normalize_inet(value)
        if tag == "cidr":
            return _net.normalize_cidr(value)
        return _net.normalize_macaddr(value)
    if tag in _BIT_TAGS:
        from secantus.sql import bitstr as _bitstr

        # Column-level coercion has no declared length here (an explicit
        # ``::bit(n)`` cast pads/truncates in the scalar evaluator); validate and
        # canonicalise the '0'/'1' string.
        return _bitstr.normalize(value, varying=(tag == "varbit"))
    if tag == "interval":
        from secantus.sql import intervals as _intervals

        return value if _intervals.is_interval(value) else _intervals.parse(str(value))
    if tag == "uuid":
        from secantus.sql import uuidtype as _uuidtype

        return _uuidtype.normalize(value)
    if tag == "money":
        from secantus.sql import numformat as _numformat

        return bson.Decimal128(_numformat.parse_money(value))
    if tag in _GEO_TAGS:
        from secantus.sql import pggeo as _pggeo

        return _pggeo.canonical(value, tag)
    if tag == "hstore":
        from secantus.sql import hstore as _hstore

        return _hstore.parse(value)
    if tag == "xml":
        from secantus.sql import xmltype as _xmltype

        return _xmltype.parse(value)
    if tag in ("int2", "int4", "oid"):
        return _int_or_22p02(value, tag)
    if tag == "oid":
        # oid is an unsigned 32-bit integer; Postgres' input/cast reinterprets a
        # negative value modulo 2^32 ((-1)::oid -> 4294967295).
        return _int_or_22p02(value, tag) & 0xFFFFFFFF
    if tag == "int8":
        return bson.Int64(_int_or_22p02(value, tag))
    if tag in ("float4", "float8"):
        try:
            return float(value)
        except (TypeError, ValueError) as e:
            raise _coercion_error(tag, value) from e
    if tag == "numeric":
        d = value if isinstance(value, Decimal) else Decimal(str(value))
        try:
            return bson.Decimal128(d)
        except _decimal.DecimalException:
            # Decimal128 holds 34 significant digits; a longer Decimal (from a
            # binary numeric parameter) rounds into range rather than erroring —
            # see tasks/backlog.md (numeric precision beyond Decimal128).
            from bson.decimal128 import create_decimal128_context

            with _decimal.localcontext(create_decimal128_context()) as ctx:
                return bson.Decimal128(ctx.create_decimal(d))
    if tag in ("text", "citext"):
        # citext stores the original text verbatim (case preserved for display);
        # the case-insensitivity is applied by the query planner, not on write.
        return str(value)
    if tag == "jsonpath":
        from secantus.sql import jsonpath as _jsonpath

        try:
            return _jsonpath.canonicalize(str(value))
        except _jsonpath.JsonPathError as exc:
            raise ValueError(str(exc)) from exc
    if tag == "ltree":
        s_ = str(value)
        if not _LTREE_RE.fullmatch(s_):
            raise ValueError(f"ltree syntax error at character 1: {s_!r}")
        return s_
    if tag == "char1":
        # PG's internal one-byte "char": input truncates to ONE character
        # (crdb's UTF-8-character rule, pinned by the pgtest char corpus),
        # and an empty string / zero byte stores NULL (the corpus reads
        # both back as SQL NULL).
        if isinstance(value, int) and not isinstance(value, bool):
            value = chr(value) if 0 <= value < 0x110000 else str(value)
        s = str(value)
        if s in ("", "\x00"):
            return None
        return s[0]
    if tag == "bool":
        if isinstance(value, str):
            # A text-format bound parameter / literal arrives as Postgres' bool
            # spelling — ``bool("f")`` would be True, so parse it properly.
            s = value.strip().lower()
            if s in ("t", "true", "yes", "on", "1"):
                return True
            if s in ("f", "false", "no", "off", "0"):
                return False
            raise ValueError(f"invalid input syntax for type boolean: {value!r}")
        return bool(value)
    if tag == "timestamptz":
        if isinstance(value, _dt.datetime):
            return _as_session_instant(value)
        # ISO-8601 string literal -> datetime (with the 3.10 short-offset net).
        from secantus.sql.datetimes import (
            datetime_sentinel,
            parse_iso_datetime,
            wide_timestamp_text,
        )

        sentinel = datetime_sentinel(value)
        if sentinel is not None:
            return sentinel
        session = _render_session.get()
        default_off = None
        if session is not None:
            from secantus.sql.datetimes import session_offset_text

            default_off = session_offset_text(session)
        wide = wide_timestamp_text(value, default_offset=default_off)
        if wide is not None:
            return wide  # PG-valid but beyond Python's datetime range: text
        return _representable_instant(_as_session_instant(parse_iso_datetime(value)))
    if tag == "timestamp":
        # Naive "without time zone". A TEXT literal carrying an offset has it
        # dropped and its wall-clock fields kept, which is Postgres' timestamp
        # INPUT rule: '1950-02-07 00:00:00+02'::timestamp is 1950-02-07
        # 00:00:00. A tz-aware datetime VALUE is different — that is a
        # timestamptz being converted, and Postgres converts it through the
        # session zone, so the same instant under America/New_York becomes
        # 1950-02-06 17:00:00. Both checked against PostgreSQL 14.13.
        #
        # Treating a bound aware parameter like a literal left it on UTC wall
        # clock, which shifted every timestamp a JDBC client wrote by the
        # session zone's offset.
        if isinstance(value, _dt.datetime) and value.tzinfo is not None:
            return _to_session_wall_clock(value)
        from secantus.sql.datetimes import (
            datetime_sentinel,
            parse_iso_datetime,
            wide_timestamp_text,
        )

        if not isinstance(value, _dt.datetime):
            sentinel = datetime_sentinel(value)
            if sentinel is not None:
                return sentinel
            # A "without time zone" column forgets any offset the input
            # carried, the same as the in-range path below does.
            wide = wide_timestamp_text(value, drop_offset=True)
            if wide is not None:
                return wide
        if isinstance(value, _dt.datetime):
            dt = value
        else:
            from secantus.sql import errors as _sql_errors

            try:
                dt = parse_iso_datetime(value)
            except _sql_errors.SQLError:
                raise  # already a typed error; do not re-wrap
            except ValueError as exc:
                # An unparseable timestamp reached the wire as "internal
                # error" — Python's ValueError escaped uncaught. Postgres
                # reports invalid input syntax, and so does this now.
                raise _coercion_error(tag, value) from exc
        return dt.replace(tzinfo=None) if dt.tzinfo is not None else dt
    if tag in ("date", "time", "timetz"):
        from secantus.sql import datetimes as _datetimes

        if tag == "date":
            return _datetimes.parse_date(value)
        if tag == "time":
            return _datetimes.parse_time(value)
        return _datetimes.parse_timetz(value)
    if tag == "json":
        parsed = _json.loads(value) if isinstance(value, str) else value
        return _bson_safe_json(parsed)
    if tag == "bytea":
        from secantus.sql import bytea as _bytea

        # Return plain ``bytes`` (not ``bson.Binary``): BSON encodes both as a
        # subtype-0 Binary, so storage is identical, but the value round-trips
        # back out of WiredTiger as plain ``bytes`` (pymongo decodes subtype-0
        # to ``bytes``). A ``bson.Binary`` filter constant does NOT compare equal
        # to a stored ``bytes`` value, so a ``WHERE data = '\x..'::bytea`` would
        # match nothing. Keeping the coerced value as ``bytes`` makes the filter
        # constant and the stored value compare equal.
        return _bytea.parse(value)
    return value


#: BSON int64 bounds — a JSON number outside them can't be stored as a BSON int.
_INT64_MIN, _INT64_MAX = -(1 << 63), (1 << 63) - 1


def _bson_safe_json(value: Any) -> Any:
    """Make a parsed JSON value BSON-encodable: an int beyond int64 range (JSON
    numbers are arbitrary-precision in Postgres) is stored as ``Decimal128``.
    ``_render_json`` renders it back as a bare number."""
    if isinstance(value, bool):
        return value
    if isinstance(value, int) and not (_INT64_MIN <= value <= _INT64_MAX):
        return bson.Decimal128(Decimal(value))
    if isinstance(value, list):
        return [_bson_safe_json(v) for v in value]
    if isinstance(value, dict):
        return {k: _bson_safe_json(v) for k, v in value.items()}
    return value


def _render_json(value: Any, compact: bool = False) -> str:
    """Render a stored JSON value as text. Identical to ``json.dumps`` except
    that a ``Decimal128`` (an int that overflowed BSON's int64 — see
    ``_bson_safe_json``) renders as a bare number, not a quoted string.

    Default spacing is jsonb's canonical form (``{"a": 1, "b": 2}`` — space
    after colon and comma, like real PG). ``compact`` drops the spaces for
    plain ``json`` columns: PG preserves a json value's input text verbatim,
    and machine-written JSON is compact, so compact re-rendering reproduces
    the typical input byte-for-byte (a hand-spaced literal still normalises —
    the parsed storage shape can't be fully verbatim; tasks/backlog.md)."""
    isep, ksep = (",", ":") if compact else (", ", ": ")
    if isinstance(value, bson.Decimal128):
        return str(value.to_decimal())
    if isinstance(value, Decimal):
        # ``to_py`` unwraps a top-level Decimal128 column value to Decimal.
        return str(value)
    if isinstance(value, list):
        return "[" + isep.join(_render_json(v, compact) for v in value) + "]"
    if isinstance(value, dict):
        return (
            "{"
            + isep.join(
                f"{_json.dumps(str(k))}{ksep}{_render_json(v, compact)}" for k, v in value.items()
            )
            + "}"
        )
    return _json.dumps(value, default=str)


#: pg_type oid of ``character(n)`` / ``bpchar``.
BPCHAR_OID = 1042


#: pg_type oid of ``character varying``.
VARCHAR_OID = 1043


def enforce_declared_length(value: Any, pg_oid: int | None, typmod: int, column: str = "") -> Any:
    """Apply a ``char(n)`` / ``varchar(n)`` declared length, Postgres-style.

    Over-length input is an ERROR — `22001 value too long for type character
    varying(3)` — not a silent truncation. The one exception is an overflow made
    only of TRAILING BLANKS, which Postgres trims to fit: `'abc  '` into a
    `varchar(3)` stores `'abc'`, while `'abcd'` is refused (both probed against
    14). A database that quietly stored a value violating its own declared
    schema would be lying about the column.

    Returns the value to store (possibly blank-trimmed); raises on overflow.
    ``atttypmod`` is the declared width + 4; anything without one is unbounded.
    """
    if pg_oid not in (BPCHAR_OID, VARCHAR_OID) or typmod <= 4 or not isinstance(value, str):
        return value
    width = typmod - 4
    if len(value) <= width:
        return value
    trimmed = value.rstrip(" ")
    if len(trimmed) <= width:
        # Only trailing blanks overflowed — Postgres trims rather than refuses.
        return value[:width] if len(trimmed) < width else trimmed
    from secantus.sql import errors

    name = "character varying" if pg_oid == VARCHAR_OID else "character"
    raise errors.SQLError(
        "22001",
        f"value too long for type {name}({width})",
        diag={"c": column} if column else None,
    )


def blank_pad(value: Any, pg_oid: int, typmod: int) -> Any:
    """Blank-pad a ``character(n)`` value to its declared width for output.

    ``char(n)`` is a *blank-padded* type: Postgres stores and sends ``'hello'``
    in a ``char(8)`` column as ``'hello   '`` (pgtest's row_description reads the
    padded DataRow). The padding is applied on the way out rather than on the way
    in, because the semantics that matter internally are the unpadded ones —
    ``length()`` ignores trailing blanks, comparison ignores them, and casting to
    ``text`` strips them, all of which fall out for free when the stored value
    stays unpadded. ``atttypmod`` is the declared width + 4; a bare ``char``
    (typmod -1) has no width to pad to.
    """
    if pg_oid != BPCHAR_OID or typmod <= 4 or not isinstance(value, str):
        return value
    width = typmod - 4
    return value.ljust(width) if len(value) < width else value


def to_pg_text(value: Any, tag: str | None = None) -> bytes | None:
    """Render a (already ``to_py``-normalised) result value as Postgres text.

    Returns ``None`` for SQL NULL (the wire layer encodes that as a -1 length
    in a ``DataRow``). This is the v3 protocol's *text* result format; the
    binary format is a later optimisation.
    """
    if value is None:
        return None
    if isinstance(value, RegClassValue):
        return value.relname.encode("utf-8")
    if tag == "float4" and isinstance(value, float):
        # float4out renders at SINGLE precision — shortest round-trip by
        # default, %.{6+efd}g when extra_float_digits is negative (pgtest
        # float corpus; array elements come through _render_pg_array).
        return _render_pg_float4(value).encode("ascii")
    if tag == "timetz" or isinstance(value, TimeTzText):
        from secantus.sql import datetimes as _datetimes

        return _datetimes.render_timetz(value).encode("ascii")
    if tag in _VECTOR_TAGS and isinstance(value, (list, tuple)):
        # int2vector / oidvector render as space-separated ints ("1 2"), the
        # form libpq clients parse for pg_index.indkey/indoption/indclass.
        return " ".join(str(int(v)) for v in value).encode("ascii")
    if is_array_tag(tag) and isinstance(value, (list, tuple)):
        elem = array_element_tag(tag)
        return _render_pg_array(value, elem).encode("utf-8")
    if tag in ("json", "json_plain"):
        # A JSON value renders as JSON text whatever its top-level type — a bare
        # ``true`` / ``"str"`` must not fall through to the bool/str renderers.
        # "json_plain" is an internal render-only tag: a plain ``json`` (oid
        # 114) column renders compact, jsonb keeps its canonical spacing. A
        # JsonText value is the client's own text, echoed VERBATIM — PG's
        # plain json preserves input bytes (``SELECT $1::JSON`` round-trips
        # exactly; the pgtest corpus compares them byte-for-byte).
        if isinstance(value, JsonText):
            return str(value).encode("utf-8")
        return _render_json(value, compact=(tag == "json_plain")).encode("utf-8")
    if isinstance(value, bool):
        return b"t" if value else b"f"
    if isinstance(value, (bytes, bytearray)):
        return b"\\x" + bytes(value).hex().encode("ascii")
    if isinstance(value, _dt.datetime):
        # Postgres renders timestamptz space-separated with a UTC offset. A stored
        # timestamptz decodes tz-naive UTC from BSON, so tag it UTC before
        # rendering — then convert to the session's TimeZone GUC like a real
        # server (the wire text carries the session-zone wall clock + offset).
        if tag == "timestamptz":
            if value.tzinfo is None:
                value = value.replace(tzinfo=_dt.timezone.utc)
            # Converting near datetime.min/max can cross Python's range — keep
            # the value as-is in that case.
            with contextlib.suppress(OverflowError, ValueError):
                value = value.astimezone(render_tzinfo())
        # ``timestamp`` (without time zone) never carries an offset — strip any
        # stray tzinfo so the wire text is naive wall-clock.
        elif tag == "timestamp" and value.tzinfo is not None:
            value = value.replace(tzinfo=None)
        if tag == "timestamp":
            style, order = render_datestyle()
            if style != "ISO":
                return _render_timestamp_style(value, style, order).encode("utf-8")
        return _render_timestamp_iso(value).encode("utf-8")
    if tag == "date":
        # Dates are stored as canonical ``YYYY-MM-DD`` text; a non-ISO DateStyle
        # reorders the fields the way psycopg's DateLoader slices them.
        style, order = render_datestyle()
        text = str(value)
        if style != "ISO" and _DATE_TEXT_RE.match(text):
            y, mo, d = (int(p) for p in text.split("-"))
            return _render_date_style(y, mo, d, style, order).encode("utf-8")
        return text.encode("utf-8")
    if tag in _RANGE_TAGS and isinstance(value, dict):
        from secantus.sql import ranges as _ranges

        return _ranges.render(value, tag).encode("utf-8")
    if tag in _MULTIRANGE_TAGS and isinstance(value, dict):
        from secantus.sql import ranges as _ranges

        return _ranges.render_multirange(value, tag).encode("utf-8")
    if tag == "tsvector" and isinstance(value, dict):
        from secantus.sql import fts as _fts

        return _fts.render_tsvector(value).encode("utf-8")
    if tag == "tsquery" and isinstance(value, dict):
        from secantus.sql import fts as _fts

        return _fts.render_tsquery(value).encode("utf-8")
    if tag in _NET_TAGS and isinstance(value, str):
        from secantus.sql import net as _net

        if tag == "inet":
            return _net.render_inet(value).encode("utf-8")
        return value.encode("utf-8")  # cidr / macaddr are stored canonical
    if tag in _BIT_TAGS and isinstance(value, str):
        return value.encode("utf-8")  # already a canonical '0'/'1' string
    if tag == "money":
        from secantus.sql import numformat as _numformat

        return _numformat.render_money(value).encode("utf-8")
    if tag == "hstore" and isinstance(value, dict):
        from secantus.sql import hstore as _hstore

        return _hstore.render(value).encode("utf-8")
    if tag == "interval" and isinstance(value, dict) and "interval" in value:
        from secantus.sql import intervals as _intervals

        return _intervals.render(value).encode("utf-8")
    if tag == "composite" and isinstance(value, dict):
        return _render_pg_composite(value).encode("utf-8")
    if isinstance(value, list):
        # A list under a non-array, non-json tag (``array[…]::text``) renders as
        # Postgres' array_out literal, not a JSON list.
        return _render_pg_array(value, infer_elem_tag(value)).encode("utf-8")
    if isinstance(value, dict):
        # A range-shaped subdoc under an unknown tag (a user-declared range
        # type's minted oid) renders as its range literal, not JSON.
        if "multirange" in value:
            from secantus.sql import ranges as _ranges

            return _ranges.render_multirange(value).encode("utf-8")
        if "empty" in value or ("lower" in value and "upper" in value):
            from secantus.sql import ranges as _ranges

            return _ranges.render(value).encode("utf-8")
        return _render_json(value).encode("utf-8")
    if isinstance(value, bson.Decimal128):
        return _render_pg_numeric(value.to_decimal()).encode("utf-8")
    if isinstance(value, Decimal):
        return _render_pg_numeric(value).encode("utf-8")
    if isinstance(value, float):
        return _render_pg_float(value).encode("ascii")
    return str(value).encode("utf-8")


def _render_pg_numeric(value: Decimal) -> str:
    """Postgres ``numeric_out`` text: plain positional notation, never
    exponent form (``Decimal('1.1E+2')`` prints ``110``, not ``1.1E+2``)."""
    if not value.is_finite():
        if value.is_nan():
            return "NaN"
        return "Infinity" if value > 0 else "-Infinity"
    return format(value, "f")


def _extra_float_digits() -> int:
    """The active session's extra_float_digits GUC (PG 12+ default 1 =
    shortest round-trip; negative values reduce %g precision)."""
    session = _render_session.get()
    if session is None:
        return 1
    try:
        return int(session.get_setting("extra_float_digits") or 1)
    except (TypeError, ValueError):
        return 1


def _render_pg_float(value: float) -> str:
    """Postgres ``float8out`` text: shortest round-trip form (no ``.0`` on an
    integral value), PG's ``NaN`` / ``Infinity`` / ``-Infinity`` spellings,
    and ``%.{15+efd}g`` when extra_float_digits is negative (pgtest float
    corpus pins efd -1 / -15)."""
    if _math.isnan(value):
        return "NaN"
    if _math.isinf(value):
        return "Infinity" if value > 0 else "-Infinity"
    efd = _extra_float_digits()
    if efd < 1:
        return f"{value:.{max(1, 15 + efd)}g}"
    s = repr(value)
    return s[:-2] if s.endswith(".0") else s


def _render_pg_float4(value: float) -> str:
    """Postgres ``float4out``: the shortest decimal that round-trips to the
    same single-precision value, or ``%.{6+efd}g`` when extra_float_digits
    is negative."""
    if _math.isnan(value):
        return "NaN"
    if _math.isinf(value):
        return "Infinity" if value > 0 else "-Infinity"
    efd = _extra_float_digits()
    if efd < 1:
        return f"{value:.{max(1, 6 + efd)}g}"
    packed = _struct.pack("!f", value)
    for p in range(1, 10):
        s = f"{value:.{p}g}"
        if _struct.pack("!f", float(s)) == packed:
            return s
    return f"{value:.9g}"


def parse_pg_record_literal(text: str) -> list[str | None]:
    """Parse a Postgres record text literal ``(a,"b,c",,\\"q\\")`` into raw field
    strings (None for an empty/NULL field). Handles double-quoted fields with
    ``""`` doubling and backslash escapes, including nested ``(…)`` records
    carried as quoted text."""
    s = text.strip()
    if not (s.startswith("(") and s.endswith(")")):
        raise ValueError(f"malformed record literal: {text!r}")
    body = s[1:-1]
    fields: list[str | None] = []
    buf: list[str] = []
    quoted = was_quoted = False
    i, n = 0, len(body)
    while i <= n:
        c = body[i] if i < n else ","  # virtual trailing comma flushes the last field
        if quoted:
            if c == "\\" and i + 1 < n:
                buf.append(body[i + 1])
                i += 2
                continue
            if c == '"':
                if i + 1 < n and body[i + 1] == '"':
                    buf.append('"')
                    i += 2
                    continue
                quoted = False
                i += 1
                continue
            buf.append(c)
            i += 1
            continue
        if c == '"':
            quoted = was_quoted = True
            i += 1
            continue
        if c == ",":
            text_field = "".join(buf)
            fields.append(text_field if (text_field or was_quoted) else None)
            buf, was_quoted = [], False
            i += 1
            continue
        if c == "\\" and i + 1 < n:
            buf.append(body[i + 1])
            i += 2
            continue
        buf.append(c)
        i += 1
    return fields


def _render_pg_composite(value: dict) -> str:
    """Render a composite value (a subdocument, already in field order) as the
    Postgres record text literal ``(f1,f2,…)``. A NULL field is empty; a field is
    double-quoted when empty or containing a comma / paren / quote / backslash /
    whitespace, with internal ``"`` and ``\\`` doubled."""
    parts: list[str] = []
    for field_val in value.values():
        if field_val is None:
            parts.append("")
            continue
        # A dict-valued field is itself a composite (nested composite type) — render
        # it recursively as a ``(…)`` record rather than as JSON.
        if isinstance(field_val, dict):
            text = _render_pg_composite(field_val)
        else:
            rendered = to_pg_text(field_val)
            text = rendered.decode("utf-8") if rendered is not None else ""
        if text == "" or any(ch in text for ch in ',()"\\') or any(ch.isspace() for ch in text):
            text = '"' + text.replace("\\", "\\\\").replace('"', '""') + '"'
        parts.append(text)
    return "(" + ",".join(parts) + ")"


def to_py(value: Any, tag: str) -> Any:
    """Render a stored BSON value back to a plain Python value for a result row.

    Normalises the BSON wrapper types (Int64, Decimal128, ObjectId, Binary) to
    their natural Python forms so result rows compare cleanly. The wire layer
    will instead format to Postgres text; this is the embedded-API view.
    """
    if value is None:
        return None
    if is_array_tag(tag) and isinstance(value, (list, tuple)):
        elem = array_element_tag(tag)
        return [to_py(v, elem) for v in value]
    if isinstance(value, bson.Int64):
        return int(value)
    if isinstance(value, bson.Decimal128):
        return value.to_decimal()
    if isinstance(value, bson.ObjectId):
        return str(value)
    if isinstance(value, bson.Binary):
        return bytes(value)
    return value


def infer_elem_tag(items: Any) -> str:
    """Best-effort element tag for rendering an untyped Python list as a PG
    array literal — keyed off the first non-None LEAF element. A
    multi-dimensional array (``flag[][]`` → nested lists) shares one element
    type across every level, so descend before deciding; keying off the outer
    list would type it ``json`` and render nested braces as quoted JSON."""
    elem = next((v for v in items if v is not None), None)
    while isinstance(elem, (list, tuple)):
        nxt = next((v for v in elem if v is not None), None)
        if nxt is None:
            break
        elem = nxt
    if isinstance(elem, bool):
        return "bool"
    if isinstance(elem, int):
        return "int8"
    if isinstance(elem, float):
        return "float8"
    if isinstance(elem, _dt.datetime):
        return "timestamptz"
    if isinstance(elem, (dict, list)):
        return "json"
    if isinstance(elem, (bson.Decimal128, _decimal.Decimal)):
        return "numeric"
    return "text"


def _render_pg_array(items: Any, elem_tag: str) -> str:
    """Render a Python list as a Postgres array text literal ``{a,b,c}``, quoting
    an element only when it needs it (empty, NULL-looking, or containing the
    delimiter / brace / quote / whitespace). ``box`` is the one built-in whose
    array delimiter is ``;`` (its own text form contains commas)."""
    delim = ";" if elem_tag == "box" else ","
    parts: list[str] = []
    for v in items:
        if v is None:
            if elem_tag == "json":
                # A JSON null element's text is ``null`` (quoted below so it
                # doesn't read as an SQL NULL element), matching a client dump.
                parts.append('"null"')
            else:
                parts.append("NULL")
            continue
        if isinstance(v, (list, tuple)) and elem_tag != "json":
            # A nested sub-array renders as bare nested braces, not a quoted
            # element (multi-dimensional array literals).
            parts.append(_render_pg_array(v, elem_tag))
            continue
        rendered = to_pg_text(v, elem_tag)
        text = rendered.decode("utf-8") if rendered is not None else ""
        # Quote on ANY whitespace (str.isspace covers \n, \r, \x1c-\x1f, …) —
        # unquoted whitespace is stripped by array-literal parsers, ours included.
        if (
            text == ""
            or text.upper() == "NULL"
            or any(c in text for c in delim + '{}"\\')
            or any(ch.isspace() for ch in text)
        ):
            escaped = text.replace("\\", "\\\\").replace('"', '\\"')
            parts.append(f'"{escaped}"')
        else:
            parts.append(text)
    return "{" + delim.join(parts) + "}"
