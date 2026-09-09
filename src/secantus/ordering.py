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

from secantus.bsontypes import bson_value_repr, regex_options_string


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
    # mongod refuses the whole sort when any document makes a numeric path
    # component ambiguous; it does not silently pick a reading.
    for d in docs:
        for f, _ in fields:
            _check_sort_path_ambiguity(d, f)
    # Single sort over a precomputed tuple key rather than N stable passes:
    # one pass through Timsort, the path resolved once per field per doc.
    return sorted(
        docs,
        key=lambda d: tuple(
            _SortKey(_sort_value(d, f, rev), reverse=rev, collation=collation) for f, rev in fields
        ),
    )


class AmbiguousSortPathError(Exception):
    """A sort path whose numeric component is BOTH an index and a field name.

    ``{x: [{"0": 5}]}`` sorted by ``x.0``: the index reading gives the element
    ``{"0": 5}`` and the field reading gives ``5``, and mongod refuses rather
    than choosing -- ``16746 Ambiguous field name found in array``. It refuses
    only for a SORT: the same path in a ``find`` filter, a ``$group`` ``_id`` or
    a projection resolves happily to both readings (measured 8.2.11,
    2026-09-08).
    """

    def __init__(self, field: str, array: list[Any]) -> None:
        rendered = ", ".join(f"{i}: {bson_value_repr(v)}" for i, v in enumerate(array))
        self.field = field
        super().__init__(
            "Ambiguous field name found in array (do not use numeric field "
            "names in embedded elements in an array), field: "
            f"'{field}' for array: {{ {rendered} }}"
        )


def _index_component(part: str) -> int | None:
    """The array index ``part`` names, or None when it names no index at all.

    Canonical digits only: mongod reads ``x.0`` as an index and ``x.00`` as a
    field name, so ``"00"`` gets no index reading and can never be ambiguous.
    """
    if not part.isdigit() or (len(part) > 1 and part[0] == "0"):
        return None
    return int(part)


def _check_sort_path_ambiguity(doc: Any, field: str) -> None:
    """Raise when a component of ``field`` names both an index and a key.

    The rule, measured over 19 shapes on 8.2.11 (2026-09-09): a component is
    ambiguous when it is a VALID INDEX of the array it is applied to *and* some
    element of that array is a document carrying that exact key. Both halves
    are load-bearing --

    * ``x.1`` over ``[{"1": 5}]`` is fine (index 1 is past the end, so only the
      field reading exists), and
    * ``x.0`` over ``[{"00": 5}]`` is fine (``"00"`` is not the key ``"0"``).

    The element carrying the key need not be the one at that index:
    ``[{a: 5}, {"0": 6}]`` sorted by ``x.0`` is refused.
    """
    _check_ambiguity(doc, field.split("."))


def _check_ambiguity(current: Any, parts: list[str]) -> None:
    if not parts:
        return
    part, rest = parts[0], parts[1:]
    if isinstance(current, Mapping):
        if part in current:
            _check_ambiguity(current[part], rest)
        return
    if isinstance(current, list):
        index = _index_component(part)
        in_range = index is not None and index < len(current)
        # The FIELD reading walks every element, which is also how a
        # non-numeric component descends through an array.
        named = [e[part] for e in current if isinstance(e, Mapping) and part in e]
        if in_range and named:
            raise AmbiguousSortPathError(part, current)
        if in_range:
            _check_ambiguity(current[index], rest)  # type: ignore[index]
        for value in named:
            _check_ambiguity(value, rest)


def _sort_value(doc: Any, field: str, reverse: bool) -> Any:
    """The value a document sorts by for one field of the sort spec.

    Resolved with :func:`_sort_path_values`, which walks a dotted path THROUGH
    an array exactly one level, rather than ``get_path``, which does not walk
    one at all. mongod ranks ``x: [{y: 1}]`` among the documents that HAVE an
    ``x.y`` -- by 1 -- and both servers ranked it with those that have none, so
    a ``sort({"x.y": 1})`` over array-of-subdocument data came back in the wrong
    order. Wrong order is wrong RESULTS as soon as a ``limit`` is involved
    (probed 8.2.11, 2026-09-06).

    One level, not any: ``x: [[{y: 5}]]`` has no ``x.y`` on mongod either, and
    `_sort_path_values` already stops there. Using it also makes the in-memory
    sort agree with the INDEX path, which generates its multikey entries from
    the same resolver -- an index must change speed, never results.

    A path yielding several values (``x: [{y: 5}, {y: 6}]``) sorts by the
    representative element the direction asks for, the same rule
    :func:`_array_sort_value` applies within one array value.
    """
    values = _sort_path_values(doc, field)
    if not values:
        # Absent: `get_path` returned None here before and still should, so a
        # missing path keeps ranking with null.
        return None
    # An array reached by an explicit INDEX is the sort value as it stands;
    # one reached by a field name is descended a level. See
    # :func:`_sort_path_values` for the measurement.
    keyed = [_SortKey(v if indexed else _array_sort_value(v, reverse)) for v, indexed in values]
    return (max(keyed) if reverse else min(keyed)).val


def _sort_path_values(doc: Any, field: str) -> list[tuple[Any, bool]]:
    """``(value, indexed)`` pairs for a sort path, ``indexed`` per value.

    Same walk as :func:`paths.get_path_values` -- both readings of a numeric
    component over an array -- but it reports, per value, whether the LAST
    component was consumed as an array INDEX. mongod descends one level into an
    array-valued sort key reached by a field name and does NOT descend one
    reached by an index, so ``{x: [[5]]}`` sorted by ``x.0`` ranks among the
    ARRAYS (its key is ``[5]``) while the same document sorted by ``x`` also
    ranks among the arrays (``[[5]]`` descended once is ``[5]``) -- and
    ``{x: [{y: [1, 2]}]}`` sorted by ``x.y`` ranks by ``1``.

    Both servers descended in every case, so ``x.0`` over ``[[5]]`` sorted as
    the NUMBER 5: wrong order, and wrong RESULTS under a ``limit``. Measured
    against 8.2.11 over seven shapes, 2026-09-09.
    """
    current: list[tuple[Any, bool]] = [(doc, False)]
    for part in field.split("."):
        nxt: list[tuple[Any, bool]] = []
        for cur, _ in current:
            if isinstance(cur, Mapping):
                if part in cur:
                    nxt.append((cur[part], False))
            elif isinstance(cur, list):
                if part.isdigit():
                    idx = int(part)
                    if 0 <= idx < len(cur):
                        nxt.append((cur[idx], True))
                for elem in cur:
                    if isinstance(elem, Mapping) and part in elem:
                        nxt.append((elem[part], False))
        current = nxt
    return current


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
