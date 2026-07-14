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

import datetime as _dt
import decimal as _decimal
import json as _json
import math as _math
from decimal import Decimal
from typing import Any

import bson
from sqlglot import exp

# Internal type tags -> Postgres type OID. Stable across the codebase; the wire
# layer reads these for RowDescription.
PG_OID: dict[str, int] = {
    "bool": 16,
    "bytea": 17,
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
    # citext (contrib): case-insensitive text. Stored (and sent on the wire) as
    # plain text — the case-folding is a query-planner behaviour, not a value shape.
    # It intentionally has NO PG_OID entry: the wire layer's ``PG_OID.get(tag, 25)``
    # already reports the text OID (25) for it, and adding an explicit ``citext: 25``
    # would collide with ``text: 25`` in the inverted OID→typename map (making
    # ``format_type(25)`` resolve to "citext" instead of "text").
    # xml (a real built-in type, OID 142). Stored as its text; validated
    # well-formed on cast / coerce.
    "xml": 142,
    # System vector types: a space-separated list of ints in text format. Used by
    # pg_index.indkey/indclass/indoption so a libpq client's catalog reflection
    # (SQLAlchemy's _SpaceVector) sees "1 2", not a JSON/array decoding.
    "int2vector": 22,
    "oidvector": 30,
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
}


def builtin_tag_for_name(name: str) -> str | None:
    """The internal tag for a *quoted* built-in type spelling, or None.

    ``CREATE TABLE t (c "cidr")`` parses the quoted name as a user-defined
    type; psycopg's own test fixtures build DDL exactly this way
    (``sql.Identifier(info.name)``), so quoted built-in names must resolve
    before the enum/domain fallback."""
    key = " ".join(str(name).strip().strip('"').lower().split())
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
        return f"{elem}[]" if elem is not None else None
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
    if base == "citext":
        return "citext"
    # ``oid`` parses as an ``exp.ObjectIdentifier`` (whose ``.sql()`` is "OID"),
    # not a DataType enum member — match on the rendered name.
    if base == "oid":
        return "oid"
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


def _parse_pg_array_literal(text: str) -> list:
    """Parse a Postgres array *string* literal (``{1,2,3}`` / ``{a,"b,c",NULL}``)
    into a Python list (a bare ``NULL`` element is None). One level deep."""
    s = text.strip()
    if not (s.startswith("{") and s.endswith("}")):
        raise ValueError(f"malformed array literal: {text!r}")
    body = s[1:-1]
    if body.strip() == "":
        return []
    out: list = []
    i, n = 0, len(body)
    # Postgres' whitespace set for array literals — NOT Python str.strip()'s:
    # \x1c-\x1f are isspace() to Python but data to Postgres.
    pg_ws = " \t\n\r\v\f"
    while i < n:
        while i < n and body[i] in pg_ws:
            i += 1
        if i < n and body[i] == '"':  # quoted element (keeps commas / literal NULL)
            i += 1
            buf: list[str] = []
            while i < n and body[i] != '"':
                if body[i] == "\\" and i + 1 < n:
                    buf.append(body[i + 1])
                    i += 2
                else:
                    buf.append(body[i])
                    i += 1
            i += 1  # closing quote
            out.append("".join(buf))
            while i < n and body[i] != ",":  # skip to the separator
                i += 1
            i += 1  # consume the comma
        else:  # unquoted element up to the next comma
            j = body.find(",", i)
            if j == -1:
                j = n
            token = body[i:j].strip(pg_ws)
            out.append(None if token.upper() == "NULL" else token)
            i = j + 1
    return out


def coerce(value: Any, tag: str) -> Any:
    """Coerce a Python literal to the BSON value stored for column ``tag``.

    ``None`` passes through as SQL NULL. Unknown tags pass the value through
    unchanged (the reflected-table path, a later phase, leans on this).
    """
    if value is None:
        return None
    if is_array_tag(tag):
        elem = array_element_tag(tag)
        items = value if isinstance(value, (list, tuple)) else _parse_pg_array_literal(str(value))
        return [coerce(v, elem) for v in items]
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
        return int(value)
    if tag == "oid":
        # oid is an unsigned 32-bit integer; Postgres' input/cast reinterprets a
        # negative value modulo 2^32 ((-1)::oid -> 4294967295).
        return int(value) & 0xFFFFFFFF
    if tag == "int8":
        return bson.Int64(int(value))
    if tag in ("float4", "float8"):
        return float(value)
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
            return value
        # ISO-8601 string literal -> datetime (with the 3.10 short-offset net).
        from secantus.sql.datetimes import parse_iso_datetime

        return parse_iso_datetime(value)
    if tag == "timestamp":
        # Naive "without time zone": an offset in the input is dropped and the
        # wall-clock fields kept (Postgres timestamp semantics), so the stored /
        # compared value is always tz-naive.
        from secantus.sql.datetimes import parse_iso_datetime

        dt = value if isinstance(value, _dt.datetime) else parse_iso_datetime(value)
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


def _render_json(value: Any) -> str:
    """Render a stored JSON value as text. Identical to ``json.dumps`` except
    that a ``Decimal128`` (an int that overflowed BSON's int64 — see
    ``_bson_safe_json``) renders as a bare number, not a quoted string."""
    if isinstance(value, bson.Decimal128):
        return str(value.to_decimal())
    if isinstance(value, Decimal):
        # ``to_py`` unwraps a top-level Decimal128 column value to Decimal.
        return str(value)
    if isinstance(value, list):
        return "[" + ", ".join(_render_json(v) for v in value) + "]"
    if isinstance(value, dict):
        return (
            "{"
            + ", ".join(f"{_json.dumps(str(k))}: {_render_json(v)}" for k, v in value.items())
            + "}"
        )
    return _json.dumps(value, default=str)


def to_pg_text(value: Any, tag: str | None = None) -> bytes | None:
    """Render a (already ``to_py``-normalised) result value as Postgres text.

    Returns ``None`` for SQL NULL (the wire layer encodes that as a -1 length
    in a ``DataRow``). This is the v3 protocol's *text* result format; the
    binary format is a later optimisation.
    """
    if value is None:
        return None
    if tag in _VECTOR_TAGS and isinstance(value, (list, tuple)):
        # int2vector / oidvector render as space-separated ints ("1 2"), the
        # form libpq clients parse for pg_index.indkey/indoption/indclass.
        return " ".join(str(int(v)) for v in value).encode("ascii")
    if is_array_tag(tag) and isinstance(value, (list, tuple)):
        elem = array_element_tag(tag)
        return _render_pg_array(value, elem).encode("utf-8")
    if tag == "json":
        # A JSON value renders as JSON text whatever its top-level type — a bare
        # ``true`` / ``"str"`` must not fall through to the bool/str renderers.
        return _render_json(value).encode("utf-8")
    if isinstance(value, bool):
        return b"t" if value else b"f"
    if isinstance(value, (bytes, bytearray)):
        return b"\\x" + bytes(value).hex().encode("ascii")
    if isinstance(value, _dt.datetime):
        # Postgres renders timestamptz space-separated with a UTC offset. A stored
        # timestamptz decodes tz-naive UTC from BSON, so tag it UTC before
        # rendering; otherwise the offset is dropped and the client parses a
        # tz-naive datetime for a timestamptz column.
        if tag == "timestamptz" and value.tzinfo is None:
            value = value.replace(tzinfo=_dt.timezone.utc)
        # ``timestamp`` (without time zone) never carries an offset — strip any
        # stray tzinfo so the wire text is naive wall-clock.
        elif tag == "timestamp" and value.tzinfo is not None:
            value = value.replace(tzinfo=None)
        return value.isoformat(sep=" ").encode("utf-8")
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
    if isinstance(value, (dict, list)):
        return _render_json(value).encode("utf-8")
    if isinstance(value, bson.Decimal128):
        return str(value.to_decimal()).encode("utf-8")
    if isinstance(value, float):
        return _render_pg_float(value).encode("ascii")
    return str(value).encode("utf-8")


def _render_pg_float(value: float) -> str:
    """Postgres ``float8out`` text: shortest round-trip form, no ``.0`` on an
    integral value (``12`` not ``12.0``), and PG's ``NaN`` / ``Infinity`` /
    ``-Infinity`` spellings (Python's are ``nan`` / ``inf``)."""
    if _math.isnan(value):
        return "NaN"
    if _math.isinf(value):
        return "Infinity" if value > 0 else "-Infinity"
    s = repr(value)
    return s[:-2] if s.endswith(".0") else s


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


def _render_pg_array(items: Any, elem_tag: str) -> str:
    """Render a Python list as a Postgres array text literal ``{a,b,c}``, quoting
    an element only when it needs it (empty, NULL-looking, or containing a comma /
    brace / quote / whitespace)."""
    parts: list[str] = []
    for v in items:
        if v is None:
            parts.append("NULL")
            continue
        rendered = to_pg_text(v, elem_tag)
        text = rendered.decode("utf-8") if rendered is not None else ""
        # Quote on ANY whitespace (str.isspace covers \n, \r, \x1c-\x1f, …) —
        # unquoted whitespace is stripped by array-literal parsers, ours included.
        if (
            text == ""
            or text.upper() == "NULL"
            or any(c in text for c in ',{}"\\')
            or any(ch.isspace() for ch in text)
        ):
            escaped = text.replace("\\", "\\\\").replace('"', '\\"')
            parts.append(f'"{escaped}"')
        else:
            parts.append(text)
    return "{" + ",".join(parts) + "}"
