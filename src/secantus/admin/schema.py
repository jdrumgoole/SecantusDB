"""Schema sampler.

Walks a list of sampled documents and reports, for every dotted-path
field encountered:

* ``count`` — how many docs had a value at this path (any type).
* ``types`` — count by BSON-friendly type name (``int``, ``string``,
  ``ObjectId``, ``datetime``, ``array``, ``object``, ``bool``, ``null``,
  ``decimal``, ``binary``, ...).
* ``null_count`` — explicit ``None`` values (separate from missing).
* ``top_values`` — up to ``TOP_VALUES`` most common scalar values with
  their counts. Skipped when the field is always an array or object.

Pure module — works on any iterable of dicts. Caller is responsible for
deciding which docs to sample (the route uses ``aggregate $sample``).
"""

from __future__ import annotations

import contextlib
import datetime as _dt
from collections import Counter
from collections.abc import Iterable
from dataclasses import dataclass, field
from decimal import Decimal
from typing import Any

from bson import Binary, Decimal128, ObjectId, Timestamp
from bson.regex import Regex

TOP_VALUES = 10


def _type_name(v: Any) -> str:
    """Map a Python value to a BSON-ish type label suitable for display."""
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, int):
        return "int"
    if isinstance(v, float):
        return "double"
    if isinstance(v, (Decimal128, Decimal)):
        return "decimal"
    if isinstance(v, str):
        return "string"
    if isinstance(v, ObjectId):
        return "ObjectId"
    if isinstance(v, _dt.datetime):
        return "datetime"
    if isinstance(v, _dt.date):
        return "date"
    if isinstance(v, Timestamp):
        return "Timestamp"
    if isinstance(v, Binary):
        return "binary"
    if isinstance(v, bytes):
        return "binary"
    if isinstance(v, Regex):
        return "regex"
    if isinstance(v, list):
        return "array"
    if isinstance(v, dict):
        return "object"
    return type(v).__name__


def _is_scalar(v: Any) -> bool:
    return not isinstance(v, (list, dict))


@dataclass
class FieldStats:
    path: str
    count: int = 0
    null_count: int = 0
    types: Counter = field(default_factory=Counter)
    values: Counter = field(default_factory=Counter)


def _walk(
    doc: Any,
    path: str,
    stats: dict[str, FieldStats],
) -> None:
    if isinstance(doc, dict):
        for k, v in doc.items():
            child = f"{path}.{k}" if path else k
            stat = stats.setdefault(child, FieldStats(path=child))
            stat.count += 1
            if v is None:
                stat.null_count += 1
                stat.types["null"] += 1
            else:
                stat.types[_type_name(v)] += 1
                if _is_scalar(v):
                    # Unhashable values (lists / dicts can't reach here
                    # but Binary etc. can) skip top-values tracking.
                    with contextlib.suppress(TypeError):
                        stat.values[v] += 1
            if isinstance(v, dict):
                _walk(v, child, stats)
            elif isinstance(v, list):
                for item in v:
                    if isinstance(item, dict):
                        _walk(item, child, stats)


def summarize(docs: Iterable[dict[str, Any]]) -> dict[str, Any]:
    """Walk an iterable of dicts and return a renderable schema summary.

    Returns:

        {
          "sample_size": int,
          "fields": [
              {
                "path": "name",
                "count": 42,
                "presence": 0.42,           # count / sample_size
                "null_count": 1,
                "types": [(type_name, n), ...],   # sorted desc
                "top_values": [(value, n), ...],  # capped at TOP_VALUES
              },
              ...
          ],
        }
    """
    docs_list = list(docs)
    sample_size = len(docs_list)
    stats: dict[str, FieldStats] = {}
    for d in docs_list:
        if isinstance(d, dict):
            _walk(d, "", stats)

    fields: list[dict[str, Any]] = []
    for path in sorted(stats.keys()):
        s = stats[path]
        types_sorted = sorted(s.types.items(), key=lambda kv: -kv[1])
        # Don't render top values when the field is structural (always
        # array / object) — they aren't meaningful comparisons.
        scalar_share = sum(n for t, n in s.types.items() if t not in ("array", "object"))
        top_vals: list[tuple[Any, int]] = []
        if scalar_share:
            top_vals = s.values.most_common(TOP_VALUES)
        fields.append(
            {
                "path": s.path,
                "count": s.count,
                "presence": (s.count / sample_size) if sample_size else 0.0,
                "null_count": s.null_count,
                "types": types_sorted,
                "top_values": top_vals,
            }
        )
    return {"sample_size": sample_size, "fields": fields}


__all__ = ["summarize", "FieldStats", "TOP_VALUES"]
