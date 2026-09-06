"""Pure BSON sort ordering — MongoDB's cross-type ``<`` and ``sort_docs``.

Extracted from ``storage.py`` so the comparator has no I/O dependency (it used
to live next to the WiredTiger code, which made ``sort_docs`` unimportable
without the ``wiredtiger`` extension). It's a pure operator engine: values in,
ordering out — the same layering as ``query`` / ``update`` / ``expressions``.
``storage`` re-exports these names for backward compatibility.
"""

from __future__ import annotations

import datetime as _dt
import math
from collections.abc import Mapping
from decimal import Decimal, InvalidOperation
from typing import Any

import bson
from bson import Binary, Code, Decimal128, MaxKey, MinKey, ObjectId, Regex, Timestamp

from secantus.bsontypes import regex_options_string
from secantus.paths import get_path


def _to_decimal(value: Any) -> Decimal:
    if isinstance(value, Decimal128):
        return value.to_decimal()
    if isinstance(value, float):
        return Decimal(repr(value))
    return Decimal(value)


def _bson_type_rank(value: Any) -> float:
    """Rank for MongoDB's cross-type sort order. Lower rank sorts first."""
    if isinstance(value, MinKey):
        return 1
    # `[]` has no element to represent it in a sort. mongod places it between
    # MinKey and null — verified: the corpus sorted `minkey < [] < null < ...`.
    if isinstance(value, _EmptyArraySortsAs):
        return 1.5  # type: ignore[return-value]
    if value is None:
        return 2
    if isinstance(value, bool):
        return 9
    # `decimal.Decimal` is here for the SQL layer, whose numerics are native
    # Decimals rather than Decimal128. Without it they fell to the catch-all
    # rank and compared by TYPE against an int -- `price < cost * 1.5` returned
    # rows where the comparison is false. It only began to matter when the
    # expression relational operators started routing through this order.
    if isinstance(value, (int, float, Decimal128, Decimal)):
        return 3
    # Before the `str` arm: `bson.Code` SUBCLASSES `str`, so an
    # `isinstance(value, str)` test catches one and used to rank every
    # JavaScript value among the strings. mongod ranks JavaScript between Regex
    # and MaxKey -- probed 8.2.11 (2026-09-01): a mixed corpus sorts
    # `... Timestamp < Regex < Code < MaxKey`.
    #
    # This must stay in step with `sortkey.RANK_JAVASCRIPT`: that encoder writes
    # the rank byte PERSISTED index entries are sorted by, this function drives
    # the in-memory sort, and moving one alone makes an index change the sort
    # answer. They were changed together, and the index-entry format version was
    # bumped so a store written before it is refused rather than read back in
    # the old order.
    if isinstance(value, Code):
        return 12.5
    if isinstance(value, str):
        return 4
    if isinstance(value, Mapping):
        return 5
    if isinstance(value, list):
        return 6
    if isinstance(value, (bytes, Binary)):
        return 7
    if isinstance(value, ObjectId):
        return 8
    if isinstance(value, _dt.datetime):
        return 10
    if isinstance(value, Timestamp):
        return 11
    if isinstance(value, Regex):
        return 12
    if isinstance(value, MaxKey):
        return 13
    return 5


def _is_nan_value(v: Any) -> bool:
    """A NaN, `float` or `Decimal128` — the one numeric value the comparison
    operators below cannot rank, because IEEE says every comparison with it is
    false."""
    if isinstance(v, bool):
        return False
    if isinstance(v, float):
        return math.isnan(v)
    if isinstance(v, Decimal128):
        try:
            return v.to_decimal().is_nan()
        except (InvalidOperation, ValueError):
            return False
    return False


class _SortKey:
    __slots__ = ("_collation", "_reverse", "val")

    def __init__(self, val: Any, reverse: bool = False, collation: Any = None) -> None:
        self.val = val
        self._reverse = reverse
        self._collation = collation

    def __lt__(self, other: _SortKey) -> bool:
        # Swap operands when this key is descending — the same comparison
        # logic then yields the correct order for desc fields, and the
        # equal-keys case still returns False on both sides (stable sort
        # preserves doc order). Both sides of the comparison must agree on
        # direction (they're in the same column), which our caller
        # guarantees.
        if self._reverse:
            a, b = other.val, self.val
        else:
            a, b = self.val, other.val
        # `Code` passes `isinstance(str)` and is UNHASHABLE, so it reached the
        # `lru_cache`d `sort_levels` and raised `TypeError: unhashable type` --
        # which `dispatch` turned into `1 internal server error` for an ordinary
        # collated sort over a collection holding a JavaScript value.
        if (
            self._collation is not None
            and isinstance(a, str)
            and isinstance(b, str)
            and not isinstance(a, Code)
            and not isinstance(b, Code)
        ):
            from secantus.collation import sort_levels

            return sort_levels(a, self._collation) < sort_levels(b, self._collation)
        return _bson_lt(a, b)

    def __eq__(self, other: object) -> bool:
        return isinstance(other, _SortKey) and self.val == other.val


def bson_equal(a: Any, b: Any) -> bool:
    """Equality by BSON semantics, where a bool is NOT a number.

    Python's ``True == 1`` is true, so ``{$eq: [true, 1]}`` answered true where
    mongod says false, and the oplog update-diff called ``{a: true}`` -> ``{a: 1}``
    no change at all. Bool and the numeric types are different BSON types; the
    numeric types do compare across themselves (``1 == 1.0`` is true on mongod),
    so only bool has to be separated out.

    Lives here rather than in either caller because both the expression language
    and the diff need exactly this rule -- the field-value rule that was copied
    into two modules and drifted is the cautionary tale.
    """
    if isinstance(a, bool) != isinstance(b, bool):
        return False
    return bool(a == b)


def bson_same_stored_value(a: Any, b: Any) -> bool:
    """Would storing ``b`` where ``a`` is leave the document unchanged?

    The CHANGE-DETECTION twin of `bson_equal`, and deliberately not the same
    predicate. mongod answers the two questions differently for a signed zero:

    * equality says they are the SAME -- ``{$eq: [0.0, -0.0]}`` is true,
      ``$cmp`` is 0, and ``find({a: -0.0})`` matches a stored ``0.0``;
    * change detection says they are DIFFERENT -- ``{$set: {a: -0.0}}`` over
      ``a: 0.0`` writes, reports ``modifiedCount: 1``, and puts the field in a
      change stream's ``updatedFields``.

    Both probed against 8.2.11 (2026-09-05). Folding this into `bson_equal`
    would have been the tempting one-line fix and would have broken `$eq` and
    query matching -- one predicate cannot serve both questions.

    The tiebreak is the ENCODED bytes, which distinguish the two zeros and also
    catch a numeric TYPE change that ``==`` hides (an ``int`` ``0`` and a
    ``float`` ``-0.0`` compare equal in Python).
    """
    if not bson_equal(a, b):
        return False
    try:
        return bson.encode({"v": a}) == bson.encode({"v": b})
    except Exception:
        # Not encodable as-is (a bare list, say). Fall back to a recursive
        # walk, which reaches a signed zero nested inside a container just as
        # the encoding would.
        if isinstance(a, list) and isinstance(b, list):
            return len(a) == len(b) and all(
                bson_same_stored_value(x, y) for x, y in zip(a, b, strict=True)
            )
        if isinstance(a, Mapping) and isinstance(b, Mapping):
            return list(a) == list(b) and all(bson_same_stored_value(a[k], b[k]) for k in a)
        return True


def _regex_sort_key(r: Regex) -> tuple[Any, str]:
    """The `(pattern, options)` pair mongod orders regexes by."""
    pattern = r.pattern
    if isinstance(pattern, bytes):
        pattern = pattern.decode("utf-8", "replace")
    return (pattern, regex_options_string(r.flags))


def _bson_lt(a: Any, b: Any) -> bool:
    """BSON sort-order ``<`` for two values.

    Handles the four cases ``__lt__`` used to inline: cross-type rank,
    Decimal128 widening, native ``<``, and the embedded-document /
    array recursion — mongo-node-driver's
    ``Aggregation ... pipeline using array`` test sorts grouped docs
    by an embedded ``_id`` field and the previous inline ``a < b``
    raised ``TypeError`` on Python's dicts.
    """
    ra = _bson_type_rank(a)
    rb = _bson_type_rank(b)
    if ra != rb:
        return ra < rb
    if a is None or b is None:
        return False
    # NaN sorts BELOW every other number, `-Infinity` included, and orders
    # EQUAL to another NaN. Neither falls out of the comparisons below: IEEE
    # makes every NaN comparison false, so `_bson_lt` answered False in both
    # directions and the sort treated a NaN as equal to whatever it happened to
    # be next to, leaving it wherever the algorithm put it -- between 5.5 and
    # Infinity in a measured case. mongod places it first among the numbers
    # (probed 8.2.11, 2026-09-06), which is also what `sortkey.encode_value`
    # already encodes for the index path; this is the in-memory twin of that.
    a_nan, b_nan = _is_nan_value(a), _is_nan_value(b)
    if a_nan or b_nan:
        return a_nan and not b_nan
    if isinstance(a, Decimal128) or isinstance(b, Decimal128):
        try:
            ad = _to_decimal(a)
            bd = _to_decimal(b)
            return bool(ad < bd)
        except (InvalidOperation, ValueError):
            pass
    # Embedded documents: compare field-by-field in insertion order,
    # first differing pair wins. Real BSON sort recurses; Python's dict
    # ``<`` raises ``TypeError`` so without this branch sort would be
    # a no-op on grouped ``_id`` keys.
    if isinstance(a, Mapping) and isinstance(b, Mapping):
        a_items = list(a.items())
        b_items = list(b.items())
        for (ak, av), (bk, bv) in zip(a_items, b_items, strict=False):
            if ak != bk:
                return ak < bk
            if _bson_lt(av, bv):
                return True
            if _bson_lt(bv, av):
                return False
        return len(a_items) < len(b_items)
    # Two regexes compare by PATTERN first, then by their option string --
    # probed 8.2.11 (2026-09-01), where a mixed corpus sorts
    # `// < /A/ < /a/ < /a/i < /a/im < /a/m < /ab/ < /b/`. `bson.Regex` defines
    # no `__lt__`, so both fell to the `TypeError` arm below and reported
    # `"Regex" < "Regex"` -- i.e. EQUAL -- and `$max` over regexes never moved.
    if isinstance(a, Regex) and isinstance(b, Regex):
        return _regex_sort_key(a) < _regex_sort_key(b)
    # BinData orders by LENGTH first, then by the bytes -- so `b"\x02"` sorts
    # BEFORE `b"\x01\x02"`, which a lexicographic compare gets backwards
    # (measured 8.2.11, 2026-09-06; the Rust server already had this right).
    # `Binary` subclasses `bytes`, so it fell to the native `<` below.
    if isinstance(a, (bytes, bytearray)) and isinstance(b, (bytes, bytearray)):
        if len(a) != len(b):
            return len(a) < len(b)
        return bytes(a) < bytes(b)
    # Arrays: lexicographic, element-by-element. Same TypeError trap
    # as the dict case for arrays-of-mixed-types.
    if isinstance(a, list) and isinstance(b, list):
        for av, bv in zip(a, b, strict=False):
            if _bson_lt(av, bv):
                return True
            if _bson_lt(bv, av):
                return False
        return len(a) < len(b)
    try:
        return bool(a < b)
    except TypeError:
        return type(a).__name__ < type(b).__name__


class _EmptyArraySortsAs:
    """Stand-in for `[]` in a sort key: below null, above MinKey (mongod)."""

    __slots__ = ()

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return "<empty-array-sort-key>"


_EMPTY_ARRAY_SORTS_AS = _EmptyArraySortsAs()


def sort_docs(
    docs: list[dict[str, Any]],
    sort_spec: Mapping[str, Any] | None,
    collation: Any = None,
) -> list[dict[str, Any]]:
    """Sort ``docs`` by ``sort_spec``, in MongoDB's cross-type order.

    ``collation`` changes how STRINGS compare (``secantus.collation
    .sort_levels``); every other type is unaffected. Passing it is what makes
    ``numericOrdering`` / accent placement / tertiary case order reach a sort at
    all -- without it a collated ``find().sort()`` silently fell back to
    codepoint order.
    """
    if not sort_spec:
        return docs
    fields = [(f, int(d) == -1) for f, d in sort_spec.items()]
    # Single sort over a precomputed tuple key rather than N stable passes:
    # one pass through Timsort, get_path called once per field per doc.
    return sorted(
        docs,
        key=lambda d: tuple(
            _SortKey(_array_sort_value(get_path(d, f), rev), reverse=rev, collation=collation)
            for f, rev in fields
        ),
    )


def _array_sort_value(v: Any, reverse: bool) -> Any:
    """mongod sorts an ARRAY-valued field by one representative element.

    Ascending takes the array's minimum element, descending its maximum —
    verified against mongod 6.0.16, where `[[1,100], [5,9], 6, [7]]` sorts
    ascending as `[1,100] < [5,9] < 6 < [7]` (by minima 1 < 5 < 6 < 7) and
    descending by maxima 100 > 9 > 7 > 6.

    Comparing whole arrays instead put every array after every scalar, which had
    a worse consequence than being merely wrong: **it disagreed with our own index
    path.** A multikey index writes one entry per element, so an IXSCAN already
    yielded mongod's element ordering, and the same query returned a different
    order depending on whether an index happened to exist. An index must change
    speed, never results.

    An empty array has no element to represent it; mongod sorts it below null
    (just above MinKey), which `_EMPTY_ARRAY_SORTS_AS` stands in for. A non-array
    value is returned unchanged.
    """
    if not isinstance(v, list):
        return v
    if not v:
        return _EMPTY_ARRAY_SORTS_AS
    keyed = [_SortKey(e) for e in v]
    return (max(keyed) if reverse else min(keyed)).val
