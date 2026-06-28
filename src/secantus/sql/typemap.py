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
}


def type_tag_for_sql(datatype: exp.DataType) -> str | None:
    """Map a parsed SQL ``DataType`` to an internal tag, or None if unknown."""
    return _DATATYPE_TAGS.get(datatype.this)


def coerce(value: Any, tag: str) -> Any:
    """Coerce a Python literal to the BSON value stored for column ``tag``.

    ``None`` passes through as SQL NULL. Unknown tags pass the value through
    unchanged (the reflected-table path, a later phase, leans on this).
    """
    if value is None:
        return None
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


def to_pg_text(value: Any) -> bytes | None:
    """Render a (already ``to_py``-normalised) result value as Postgres text.

    Returns ``None`` for SQL NULL (the wire layer encodes that as a -1 length
    in a ``DataRow``). This is the v3 protocol's *text* result format; the
    binary format is a later optimisation.
    """
    if value is None:
        return None
    if isinstance(value, bool):
        return b"t" if value else b"f"
    if isinstance(value, (bytes, bytearray)):
        return b"\\x" + bytes(value).hex().encode("ascii")
    if isinstance(value, _dt.datetime):
        # Postgres renders timestamptz space-separated with a UTC offset.
        return value.isoformat(sep=" ").encode("utf-8")
    if isinstance(value, (dict, list)):
        return _json.dumps(value, default=str).encode("utf-8")
    return str(value).encode("utf-8")


def to_py(value: Any, tag: str) -> Any:
    """Render a stored BSON value back to a plain Python value for a result row.

    Normalises the BSON wrapper types (Int64, Decimal128, ObjectId, Binary) to
    their natural Python forms so result rows compare cleanly. The wire layer
    will instead format to Postgres text; this is the embedded-API view.
    """
    if value is None:
        return None
    if isinstance(value, bson.Int64):
        return int(value)
    if isinstance(value, bson.Decimal128):
        return value.to_decimal()
    if isinstance(value, bson.ObjectId):
        return str(value)
    if isinstance(value, bson.Binary):
        return bytes(value)
    return value
