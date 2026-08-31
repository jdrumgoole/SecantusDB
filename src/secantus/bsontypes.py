"""mongod's type vocabulary — the one place that names a BSON type.

Error messages across every engine quote the type of an offending value
(``BSON field 'count.skip' is the wrong type 'objectId'``). That vocabulary is
mongod's, not Python's, and it had drifted into **three** partial copies —
`update`, `expressions` and `changestreams` each covered the types its own
messages happened to hit and fell through to ``type(v).__name__`` for the rest.
That fallback is what put Python class names on the wire: ``'ObjectId'`` for
``objectId``, ``'datetime'`` for ``date``, ``'bytes'`` for ``binData``.

Every name below was probed against mongod 8.2.11 (2026-08-31) by sending the
value into a numeric slot and reading back the type it named::

    ObjectId -> objectId    Binary/bytes -> binData    Code -> javascript
    datetime -> date        Regex/re.Pattern -> regex  Timestamp -> timestamp
    MinKey -> minKey        MaxKey -> maxKey           DBRef -> object

Two orderings are load-bearing, because pymongo subclasses builtins:

* ``Code`` is a subclass of ``str`` — check it FIRST or javascript reads as
  string;
* ``Binary`` is a subclass of ``bytes``, and ``bool`` of ``int``.

`crates/secantus-core/src/query.rs::bson_type_name` is the Rust counterpart and
already had these names; this module brings Python to it rather than the other
way round.
"""

from __future__ import annotations

import datetime as _dt
import re as _re
from collections.abc import Mapping
from typing import Any

import bson


def bson_type_name(v: Any) -> str:
    """Name ``v``'s BSON type the way mongod does in an error message."""
    # bool before int: bool is an int subclass in Python, not in BSON.
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, bson.Int64):
        return "long"
    if isinstance(v, int):
        # An int outside the int32 range encodes as an int64, and mongod names
        # it 'long'. The changestreams copy said 'int' for every int.
        return "int" if -(2**31) <= v < 2**31 else "long"
    if isinstance(v, float):
        return "double"
    if isinstance(v, bson.Decimal128):
        return "decimal"
    # Code before str (subclass), Binary before bytes (subclass).
    if isinstance(v, bson.Code):
        return "javascript"
    if isinstance(v, str):
        return "string"
    if v is None:
        return "null"
    if isinstance(v, bson.ObjectId):
        return "objectId"
    if isinstance(v, _dt.datetime):
        return "date"
    if isinstance(v, (bson.Binary, bytes, bytearray)):
        return "binData"
    if isinstance(v, (bson.Regex, _re.Pattern)):
        return "regex"
    if isinstance(v, bson.Timestamp):
        return "timestamp"
    if isinstance(v, bson.MinKey):
        return "minKey"
    if isinstance(v, bson.MaxKey):
        return "maxKey"
    # A DBRef encodes as a subdocument, and mongod calls it 'object'.
    if isinstance(v, bson.DBRef):
        return "object"
    if isinstance(v, Mapping):
        return "object"
    if isinstance(v, (list, tuple)):
        return "array"
    return type(v).__name__


def render_bson(v: Any) -> str:
    """Render a value the way mongod echoes it back inside an error message.

    `createIndexes` quotes the offending index spec verbatim —
    ``Error in specification { key: { a: 1 }, name: "i", collation: 5 } ::
    caused by :: ...`` — in mongod's shell syntax, which is neither JSON nor
    Python's ``repr``. Every rule below was probed against 8.2.11 (2026-08-31)
    by sending the value and reading the echo:

        5 / Int64(7)     5 / 7          {}                  {}
        1.5 / 2.0        1.5 / 2.0      {a: 1}              { a: 1 }
        1e30             1e+30          []                  []
        Decimal("2.5")   2.5            [1, 2]              [ 1, 2 ]
        "x" / ""         "x" / ""       ObjectId(...)       ObjectId('...')
        True / None      true / null    datetime            new Date(<ms>)
        Binary(b"\\xab")  BinData(0, AB) Binary(..., 4)      UUID("...")
        Regex("^a","i")  /^a/i          Code("f(){}")       f(){}
        Timestamp(1, 2)  Timestamp(1, 2) MinKey/MaxKey      MinKey/MaxKey

    Two details are deliberate, not oversights:

    * a non-empty document or array gets INNER spaces (``{ a: 1 }``,
      ``[ 1, 2 ]``) while an empty one does not (``{}``, ``[]``);
    * strings are **not escaped** — mongod emits ``"he said "hi""`` for a
      string containing quotes, and ``"a\\b"`` for one containing a backslash.
      Reproducing that means not calling a JSON encoder here.

    Python's ``repr`` already matches mongod for every float probed (``2.0``
    keeps its ``.0``, ``1e30`` renders ``1e+30``), so floats pass through it.
    """
    if isinstance(v, bool):
        return "true" if v else "false"
    if v is None:
        return "null"
    if isinstance(v, bson.Int64):
        return str(int(v))
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, bson.Decimal128):
        return str(v)
    if isinstance(v, bson.Code):
        return str(v)
    if isinstance(v, str):
        return f'"{v}"'
    if isinstance(v, bson.ObjectId):
        return f"ObjectId('{v}')"
    if isinstance(v, _dt.datetime):
        return f"new Date({_epoch_millis(v)})"
    if isinstance(v, (bson.Binary, bytes, bytearray)):
        subtype = getattr(v, "subtype", 0)
        if subtype == 4:
            return f'UUID("{bson.Binary(bytes(v), 4).as_uuid()}")'
        return f"BinData({subtype}, {bytes(v).hex().upper()})"
    if isinstance(v, bson.Regex):
        return f"/{v.pattern}/{_regex_flag_string(v.flags)}"
    if isinstance(v, _re.Pattern):
        return f"/{v.pattern}/{_regex_flag_string(v.flags)}"
    if isinstance(v, bson.Timestamp):
        return f"Timestamp({v.time}, {v.inc})"
    if isinstance(v, bson.MinKey):
        return "MinKey"
    if isinstance(v, bson.MaxKey):
        return "MaxKey"
    if isinstance(v, Mapping):
        if not v:
            return "{}"
        inner = ", ".join(f"{k}: {render_bson(val)}" for k, val in v.items())
        return "{ " + inner + " }"
    if isinstance(v, (list, tuple)):
        if not v:
            return "[]"
        return "[ " + ", ".join(render_bson(x) for x in v) + " ]"
    return str(v)


def _epoch_millis(v: _dt.datetime) -> int:
    """Milliseconds since the epoch, the way BSON stores a date.

    A naive datetime is UTC here, matching how pymongo encodes one.
    """
    if v.tzinfo is None:
        v = v.replace(tzinfo=_dt.timezone.utc)
    return int(v.timestamp() * 1000)


def _regex_flag_string(flags: Any) -> str:
    """mongod's trailing regex flags, in its own order (``imxlsu``)."""
    if isinstance(flags, int):
        out = ""
        for bit, ch in ((_re.I, "i"), (_re.M, "m"), (_re.X, "x"), (_re.S, "s"), (_re.U, "u")):
            if flags & bit:
                out += ch
        return out
    return str(flags or "")
