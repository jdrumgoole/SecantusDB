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
