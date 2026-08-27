"""Sub-millisecond precision for SQL ``timestamp`` columns.

BSON has no sub-millisecond date. A BSON ``Date`` (type ``0x09``) is a signed
int64 count of MILLISECONDS since the epoch, and BSON ``Timestamp`` (``0x11``)
is coarser still — seconds plus an ordinal, an internal replication type. So a
Postgres ``timestamp``, which carries microseconds, cannot round-trip through a
BSON date: ``12:00:00.123456`` decodes as ``12:00:00.123000``, silently.

The representation here keeps the ``Date`` and stores the lost remainder beside
it, so both protocols stay honest:

* a Mongo client reading the collection still sees a real BSON ``Date`` (the
  value truncated to the millisecond, exactly what it saw before);
* the SQL layer adds the remainder back on read, so ``SELECT`` returns the
  microseconds that were inserted.

The remainder lives in a ``__``-prefixed hidden field — the same convention
expression indexes use (`catalog.ExprIndex`), so it is not a table column and
never appears in ``SELECT *`` or reflection. It holds 0-999 microseconds and is
written ONLY when non-zero, so a whole-millisecond timestamp (the common case)
leaves no extra field in the document.

THE INVARIANT — read this before touching a write path
------------------------------------------------------
**Every write of a timestamp field must set or clear its companion.** A stale
companion is worse than truncation: it silently reports a time that was never
stored. A path that writes the date and forgets the companion leaves the
previous row's microseconds attached to the new value.

`carry_subms` (for a document being built) and `subms_update_ops` (for an
``$set``) both make the clearing explicit rather than leaving it to the caller
to remember, and every write path is expected to go through one of them.
"""

from __future__ import annotations

import datetime as _dt
from typing import Any

#: Type tags whose values are BSON dates and therefore lose microseconds.
SUBMS_TAGS = frozenset({"timestamp", "timestamptz"})

#: Prefix for the hidden companion field. `__`-prefixed keys are the project's
#: convention for storage fields that are not table columns.
_PREFIX = "__us_"


def companion_field(field: str) -> str:
    """The hidden field carrying ``field``'s sub-millisecond remainder."""
    return f"{_PREFIX}{field}"


def is_companion_field(name: str) -> bool:
    """Whether ``name`` is one of these hidden remainder fields (so reflection
    and ``SELECT *`` can skip it)."""
    return name.startswith(_PREFIX)


def split(value: Any) -> tuple[Any, int]:
    """``(value_as_stored, remainder_microseconds)``.

    The stored value is truncated to whole milliseconds — what BSON would do
    anyway — and the remainder is the 0-999 microseconds that would be lost.
    Anything that is not a datetime passes through with remainder 0.
    """
    if not isinstance(value, _dt.datetime):
        return value, 0
    remainder = value.microsecond % 1000
    if not remainder:
        return value, 0
    return value.replace(microsecond=value.microsecond - remainder), remainder


def merge(value: Any, remainder: Any) -> Any:
    """``value`` with ``remainder`` microseconds added back.

    Defensive about the stored remainder: a value that is not an int in 0-999 is
    ignored rather than trusted, so a hand-edited or foreign document cannot
    produce a nonsensical time.
    """
    if not isinstance(value, _dt.datetime) or not isinstance(remainder, int):
        return value
    if isinstance(remainder, bool) or not 0 < remainder < 1000:
        return value
    return value.replace(microsecond=value.microsecond + remainder)


def carry_subms(doc: dict[str, Any], field: str, value: Any) -> Any:
    """Record ``value``'s remainder for ``field`` in ``doc``; return the value to
    store.

    Always resolves the companion — writing it when there is a remainder and
    REMOVING it when there is not — so a field overwritten with a
    whole-millisecond value cannot keep the old one (the invariant above).
    """
    stored, remainder = split(value)
    companion = companion_field(field)
    if remainder:
        doc[companion] = remainder
    else:
        doc.pop(companion, None)
    return stored


def subms_update_ops(field: str, value: Any) -> tuple[Any, str, int | None]:
    """For an ``$set`` of ``field``: ``(value_to_set, companion, remainder)``.

    ``remainder`` is None when the companion must be UNSET instead of set — an
    update to a whole-millisecond value has to clear any remainder the row
    already carried.
    """
    stored, remainder = split(value)
    return stored, companion_field(field), (remainder or None)


def restore_doc(doc: dict[str, Any], fields: dict[str, str]) -> None:
    """In place, add each remainder back onto its date field.

    ``fields`` maps a storage field to its companion (only timestamp columns
    need entries). Companion keys are left in the document — callers project the
    columns they want, and `is_companion_field` keeps them out of ``SELECT *``.
    """
    for field, companion in fields.items():
        remainder = doc.get(companion)
        if remainder and field in doc:
            doc[field] = merge(doc[field], remainder)


def has_subms(value: Any) -> bool:
    """Whether ``value`` carries microseconds a BSON date could not hold — the
    signal that a comparison against it cannot be answered by the stored date
    alone."""
    return isinstance(value, _dt.datetime) and bool(value.microsecond % 1000)


def _companion_cmp(companion: str, op: str, remainder: int) -> dict[str, Any]:
    """Compare the *companion* against ``remainder``, honouring the invariant
    that an ABSENT companion means a remainder of zero.

    Mongo's `{f: None}` matches missing-or-null, which is how "remainder is 0"
    is expressed; `$gt` / `$lt` on a missing field match nothing, so the
    zero-remainder cases have to be spelled out rather than left to the
    operator.
    """
    if op == "$eq":
        # Zero remainder: the companion must be absent, not merely <= 0. This is
        # the false-positive half of the bug — `t = '…123'` matched a row storing
        # `…123456`, because only the truncated field was compared.
        return {companion: remainder} if remainder else {companion: None}
    if op == "$gt":
        # Nothing is greater than the maximum, and a missing companion (0) is
        # never greater than a remainder >= 0.
        return {companion: {"$gt": remainder}}
    if op == "$gte":
        if remainder == 0:
            return {}  # every remainder is >= 0
        return {companion: {"$gte": remainder}}
    if op == "$lt":
        if remainder == 0:
            return _MATCH_NOTHING  # no remainder is < 0
        return {"$or": [{companion: {"$lt": remainder}}, {companion: None}]}
    if op == "$lte":
        if remainder == 0:
            return {companion: None}  # only a zero remainder is <= 0
        return {"$or": [{companion: {"$lte": remainder}}, {companion: None}]}
    raise ValueError(f"unsupported companion comparison {op!r}")


#: A filter that matches no document (Mongo has no literal false).
_MATCH_NOTHING: dict[str, Any] = {"$nor": [{}]}


def cmp_filter(field: str, op: str, value: Any) -> dict[str, Any] | None:
    """Lower ``field <op> value`` so the sub-millisecond remainder participates.

    Returns ``None`` when the plain filter is already correct — a non-datetime
    value, or one that lands on a whole millisecond for an operator whose
    truncated form is exact.

    Comparisons used to see only the truncated field, which was wrong in *both*
    directions against a stored `…00.123456`: `= '…123456'` matched nothing (a
    row failing an equality on its own stored value) while `= '…123'` matched
    it. Ordering compares the millisecond first and the remainder only within
    the same millisecond, which is what these `$or` shapes say.
    """
    if not isinstance(value, _dt.datetime):
        return None
    trunc, remainder = split(value)
    companion = companion_field(field)

    if op == "$eq":
        return {"$and": [{field: trunc}, _companion_cmp(companion, "$eq", remainder)]}
    if op == "$ne":
        # `<>` must still exclude NULL/missing rows, which the caller guards;
        # here we only negate the positive match.
        return {"$nor": [{"$and": [{field: trunc}, _companion_cmp(companion, "$eq", remainder)]}]}
    if op in ("$gt", "$gte"):
        same_ms = _companion_cmp(companion, op, remainder)
        return {
            "$or": [
                {field: {"$gt": trunc}},
                {"$and": [{field: trunc}, same_ms]} if same_ms else {field: trunc},
            ]
        }
    if op in ("$lt", "$lte"):
        same_ms = _companion_cmp(companion, op, remainder)
        if same_ms == _MATCH_NOTHING:
            return {field: {"$lt": trunc}}
        return {
            "$or": [
                {field: {"$lt": trunc}},
                {"$and": [{field: trunc}, same_ms]} if same_ms else {field: trunc},
            ]
        }
    return None
