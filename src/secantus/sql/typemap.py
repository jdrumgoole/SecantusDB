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
import json as _json
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
    "int4": 23,
    "text": 25,
    "float8": 701,
    "timestamptz": 1184,
    "numeric": 1700,
    "json": 3802,
    # A composite (row) value renders as the ``(f1,f2,…)`` record text literal. We
    # report the generic RECORD pseudo-type OID rather than minting a per-type OID.
    "composite": 2249,
    # Range types render as the ``[lower,upper)`` text form.
    "int4range": 3904,
    "numrange": 3906,
    "tsrange": 3908,
    "int8range": 3926,
    "daterange": 3912,
    # Multirange types render as the ``{[a,b), [c,d)}`` text form.
    "int4multirange": 4451,
    "nummultirange": 4532,
    "tsmultirange": 4533,
    "datemultirange": 4535,
    "int8multirange": 4536,
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

# Type tags whose value is a list rendered as a Postgres ``int2vector`` /
# ``oidvector`` (space-separated, not array braces / JSON).
_VECTOR_TAGS = frozenset({"int2vector", "oidvector"})

# Range type tags — stored as a subdocument, rendered as ``[lower,upper)``.
_RANGE_TAGS = frozenset({"int4range", "int8range", "numrange", "tsrange", "daterange"})

# Multirange type tags — stored as ``{"multirange": [range, …]}``, rendered as
# ``{[a,b), [c,d)}``.
_MULTIRANGE_TAGS = frozenset(
    {"int4multirange", "int8multirange", "nummultirange", "tsmultirange", "datemultirange"}
)

# Internal type tag -> SQL type name (for information_schema.columns.data_type
# and any place a human-facing type spelling is needed).
SQL_TYPE_NAME: dict[str, str] = {
    "int4": "integer",
    "int8": "bigint",
    "float8": "double precision",
    "numeric": "numeric",
    "text": "text",
    "bool": "boolean",
    "timestamptz": "timestamp with time zone",
    "json": "jsonb",
    "bytea": "bytea",
    "composite": "record",
    "bit": "bit",
    "varbit": "bit varying",
    **{t: t for t in _RANGE_TAGS},
    **{t: t for t in _MULTIRANGE_TAGS},
    **{t: t for t in _FTS_TAGS},
    **{t: t for t in _NET_TAGS},
}

# Internal type tag -> Postgres pg_type.typname (for pg_catalog.pg_type rows).
PG_TYPENAME: dict[str, str] = {
    "int4": "int4",
    "int8": "int8",
    "float8": "float8",
    "numeric": "numeric",
    "text": "text",
    "bool": "bool",
    "timestamptz": "timestamptz",
    "json": "jsonb",
    "bytea": "bytea",
    "composite": "record",
    "bit": "bit",
    "varbit": "varbit",
    **{t: t for t in _RANGE_TAGS},
    **{t: t for t in _MULTIRANGE_TAGS},
    **{t: t for t in _FTS_TAGS},
    **{t: t for t in _NET_TAGS},
}

# sqlglot DataType.Type -> our type tag. Several SQL spellings collapse onto one
# tag (varchar/char -> text; double/float -> float8) the way Postgres widens
# them for storage purposes here.
_DATATYPE_TAGS: dict[Any, str] = {
    exp.DataType.Type.BIGINT: "int8",
    exp.DataType.Type.INT: "int4",
    exp.DataType.Type.SMALLINT: "int4",
    exp.DataType.Type.TINYINT: "int4",
    exp.DataType.Type.FLOAT: "float8",
    exp.DataType.Type.DOUBLE: "float8",
    exp.DataType.Type.DECIMAL: "numeric",
    exp.DataType.Type.BIGDECIMAL: "numeric",
    exp.DataType.Type.TEXT: "text",
    exp.DataType.Type.VARCHAR: "text",
    exp.DataType.Type.CHAR: "text",
    exp.DataType.Type.NCHAR: "text",
    exp.DataType.Type.NVARCHAR: "text",
    exp.DataType.Type.BOOLEAN: "bool",
    exp.DataType.Type.DATE: "timestamptz",
    exp.DataType.Type.DATETIME: "timestamptz",
    exp.DataType.Type.TIMESTAMP: "timestamptz",
    exp.DataType.Type.TIMESTAMPTZ: "timestamptz",
    exp.DataType.Type.JSON: "json",
    exp.DataType.Type.JSONB: "json",
    exp.DataType.Type.BINARY: "bytea",
    exp.DataType.Type.VARBINARY: "bytea",
    exp.DataType.Type.BIT: "bit",
}


# Element tag -> Postgres array type OID (pg_type of the ``_elem`` array type).
_ARRAY_PG_OID: dict[str, int] = {
    "bool": 1000,
    "int8": 1016,
    "int4": 1007,
    "text": 1009,
    "float8": 1022,
    "numeric": 1231,
    "timestamptz": 1185,
    "bytea": 1001,
}

# Register the array tags (``text[]`` -> 1009, ...) in PG_OID so the wire layer's
# RowDescription reports the array type OID for an array column, not the element's.
PG_OID.update({f"{elem}[]": oid for elem, oid in _ARRAY_PG_OID.items()})


def type_tag_for_sql(datatype: exp.DataType) -> str | None:
    """Map a parsed SQL ``DataType`` to an internal tag, or None if unknown. An
    ``ARRAY`` type becomes ``<elem>[]`` (e.g. ``int4[]``)."""
    if datatype.this == exp.DataType.Type.ARRAY:
        inner = datatype.args.get("expressions") or []
        elem = type_tag_for_sql(inner[0]) if inner else None
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
    return None


def is_array_tag(tag: str | None) -> bool:
    return isinstance(tag, str) and tag.endswith("[]")


def array_element_tag(tag: str) -> str:
    """The element tag of an array tag (``int4[]`` -> ``int4``)."""
    return tag[:-2]


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
    while i < n:
        while i < n and body[i] in " \t":
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
            token = body[i:j].strip()
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
    if tag == "int4":
        return int(value)
    if tag == "int8":
        return bson.Int64(int(value))
    if tag == "float8":
        return float(value)
    if tag == "numeric":
        return bson.Decimal128(value if isinstance(value, Decimal) else Decimal(str(value)))
    if tag == "text":
        return str(value)
    if tag == "bool":
        return bool(value)
    if tag == "timestamptz":
        if isinstance(value, _dt.datetime):
            return value
        # ISO-8601 string literal -> datetime. fromisoformat handles offsets
        # and a trailing 'Z' on 3.11+.
        return _dt.datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    if tag == "json":
        return _json.loads(value) if isinstance(value, str) else value
    if tag == "bytea":
        if isinstance(value, (bytes, bytearray)):
            return bson.Binary(bytes(value))
        return bson.Binary(bytes.fromhex(str(value)))
    return value


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
    if isinstance(value, bool):
        return b"t" if value else b"f"
    if isinstance(value, (bytes, bytearray)):
        return b"\\x" + bytes(value).hex().encode("ascii")
    if isinstance(value, _dt.datetime):
        # Postgres renders timestamptz space-separated with a UTC offset.
        return value.isoformat(sep=" ").encode("utf-8")
    if tag in _RANGE_TAGS and isinstance(value, dict):
        from secantus.sql import ranges as _ranges

        return _ranges.render(value).encode("utf-8")
    if tag in _MULTIRANGE_TAGS and isinstance(value, dict):
        from secantus.sql import ranges as _ranges

        return _ranges.render_multirange(value).encode("utf-8")
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
    if tag == "composite" and isinstance(value, dict):
        return _render_pg_composite(value).encode("utf-8")
    if isinstance(value, (dict, list)):
        return _json.dumps(value, default=str).encode("utf-8")
    return str(value).encode("utf-8")


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
        if text == "" or text.upper() == "NULL" or any(c in text for c in ',{}"\\ \t'):
            escaped = text.replace("\\", "\\\\").replace('"', '\\"')
            parts.append(f'"{escaped}"')
        else:
            parts.append(text)
    return "{" + ",".join(parts) + "}"
