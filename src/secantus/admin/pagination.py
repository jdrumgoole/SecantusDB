"""Skip-ID pagination for the collection viewer.

The contract is intentionally narrow: results are sorted by ``_id``
(ascending or descending), and each page returns a cursor token that
identifies the last ``_id`` seen. The next page asks the server for
``_id > <last>`` (or ``< <last>`` when descending). Stateless on both
sides — no server-held cursor, no idle timeout, no race.

Limitations of this v1:

* Filter MUST NOT include ``_id``. Combining a user-supplied ``_id``
  filter with the cursor's range clause leads to confusing semantics
  (single-doc filters are inherently unpaginatable; range filters
  collide with the cursor's range). Custom-sort + filter-on-``_id``
  pagination needs real-pymongo-cursor pagination, which lands in a
  later slice.
* Only ``_id`` types we encode round-trippably are ``ObjectId``,
  ``int``, and ``str``. ``Decimal128`` / ``UUID`` / ``Binary`` ``_id``s
  raise ``ValueError`` at cursor-encode time, before the response ships.
"""

from __future__ import annotations

import base64
import json
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from bson import ObjectId


@dataclass(frozen=True)
class PageCursor:
    """The opaque resume position for skip-ID pagination."""

    after: Any
    type_tag: str  # "oid" | "int" | "str"


def detect_id_type(v: Any) -> str:
    """Return the cursor type tag for a value or raise ``ValueError``."""
    if isinstance(v, ObjectId):
        return "oid"
    # Reject bool *before* int — bool is a subclass of int in Python.
    if isinstance(v, bool):
        raise ValueError("bool _id is not supported for skip-ID pagination")
    if isinstance(v, int):
        return "int"
    if isinstance(v, str):
        return "str"
    raise ValueError(f"unsupported _id type for pagination: {type(v).__name__}")


def _serialize(v: Any, type_tag: str) -> str:
    if type_tag == "oid":
        return str(v)
    if type_tag == "int":
        return str(int(v))
    if type_tag == "str":
        return v
    raise ValueError(f"unknown cursor type: {type_tag}")


def _deserialize(s: str, type_tag: str) -> Any:
    if type_tag == "oid":
        return ObjectId(s)
    if type_tag == "int":
        return int(s)
    if type_tag == "str":
        return s
    raise ValueError(f"unknown cursor type: {type_tag}")


def encode_cursor(cursor: PageCursor) -> str:
    """Encode a ``PageCursor`` as a URL-safe opaque token."""
    payload = {
        "after": _serialize(cursor.after, cursor.type_tag),
        "type": cursor.type_tag,
    }
    raw = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    encoded = base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")
    return encoded


def decode_cursor(token: str | None) -> PageCursor | None:
    """Decode an opaque token back into a ``PageCursor``.

    Returns ``None`` when ``token`` is ``None`` or empty (= "first page").
    Raises ``ValueError`` on malformed tokens.
    """
    if not token:
        return None
    try:
        padded = token + "=" * (-len(token) % 4)
        raw = base64.urlsafe_b64decode(padded.encode("ascii"))
        payload = json.loads(raw.decode("utf-8"))
    except (ValueError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise ValueError("malformed page cursor") from exc
    if not isinstance(payload, dict):
        raise ValueError("malformed page cursor")
    type_tag = payload.get("type")
    after_raw = payload.get("after")
    if not isinstance(type_tag, str) or after_raw is None:
        raise ValueError("malformed page cursor")
    return PageCursor(after=_deserialize(str(after_raw), type_tag), type_tag=type_tag)


def build_page_filter(
    base_filter: Mapping[str, Any] | None,
    cursor: PageCursor | None,
    *,
    sort_dir: int,
) -> dict[str, Any]:
    """Combine the user filter with the cursor's range clause.

    Raises ``ValueError`` if ``base_filter`` includes a ``_id`` key — see
    the module docstring for the rationale.
    """
    if sort_dir not in (1, -1):
        raise ValueError("sort_dir must be 1 or -1")
    final: dict[str, Any] = dict(base_filter or {})
    if "_id" in final:
        raise ValueError("filter must not include _id when using skip-ID pagination")
    if cursor is not None:
        op = "$gt" if sort_dir == 1 else "$lt"
        final["_id"] = {op: cursor.after}
    return final


def make_next_cursor(rows: list[dict[str, Any]], page_size: int) -> str | None:
    """Return the encoded next-cursor token, or ``None`` if exhausted.

    The caller passes the over-fetched batch (``page_size + 1``); if
    only ``page_size`` came back, the page is the last one.
    """
    if len(rows) <= page_size:
        return None
    last = rows[page_size - 1]
    type_tag = detect_id_type(last["_id"])
    return encode_cursor(PageCursor(after=last["_id"], type_tag=type_tag))


def encode_doc_id(value: Any) -> str:
    """URL-safe round-trip encoding for an ``_id`` value.

    Reuses the page-cursor encoding so URL tokens are uniform across
    pagination and document edit/delete routes.
    """
    return encode_cursor(PageCursor(after=value, type_tag=detect_id_type(value)))


def decode_doc_id(token: str) -> Any:
    """Inverse of :func:`encode_doc_id`. Raises ``ValueError`` on bad input."""
    cursor = decode_cursor(token)
    if cursor is None:
        raise ValueError("missing document id token")
    return cursor.after


__all__ = [
    "PageCursor",
    "detect_id_type",
    "encode_cursor",
    "decode_cursor",
    "build_page_filter",
    "make_next_cursor",
    "encode_doc_id",
    "decode_doc_id",
]
