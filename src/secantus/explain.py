"""mongod's ``explain`` output shapes: the normalised query and the stage tree.

Two things live here, both of them pure functions over already-parsed input so
they can be unit-tested without a server:

``canonical_match``
    mongod does not echo the filter you sent back in ``queryPlanner
    .parsedQuery`` (nor in a stage's ``filter``) -- it echoes the
    ``MatchExpression`` tree *after* normalisation. Bare equality grows an
    explicit ``$eq``, several top-level fields become an ``$and`` whose children
    are sorted, ``$ne`` becomes ``$not``/``$eq``, ``$type`` becomes a list of
    numeric BSON codes, and so on.

``build_stage_tree``
    mongod wraps the scan in the stages that describe the query -- ``SORT``,
    ``SKIP``, ``LIMIT``, ``PROJECTION_SIMPLE`` / ``PROJECTION_DEFAULT`` -- rather
    than reporting a single flat node. The most useful consequence: a client
    asking "is my sort served by an index?" reads it off the presence of a
    blocking ``SORT`` stage.

**Every rule here was measured against mongod 8.2.11 on 2026-09-01**, not read
off documentation -- the child ORDER inside ``$and`` in particular is mongod's
internal ``MatchExpression`` type ordinal, which is not documented anywhere and
was derived from 91 pairwise probes (``tools/probes/explain_shapes.py``). Where
a rule was not measured the input is passed through unchanged, deliberately: a
half-right normaliser reads as authoritative while being wrong.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

__all__ = [
    "MATCH_TYPE_RANK",
    "SORT_MEM_LIMIT_BYTES",
    "build_stage_tree",
    "canonical_match",
    "projection_stage_name",
]

#: mongod's blocking-sort memory budget, reported verbatim as the ``SORT``
#: stage's ``memLimit`` (``internalQueryMaxBlockingSortMemoryUsageBytes``,
#: 100 MiB -- the default, which is also what ``serverParameters`` reports).
SORT_MEM_LIMIT_BYTES = 104857600

#: The order ``$and``'s children come back in. mongod sorts them by the
#: ``MatchExpression`` type ordinal, then by path -- so ``{a: {$gt: 1}, b: 2}``
#: reports ``b``'s equality FIRST, and ``{a: {$gt: 3, $lt: 9}}`` reports ``$lt``
#: before ``$gt``. Derived pairwise against 8.2.11 rather than from the enum in
#: mongod's source, because the two disagree about where ``$not`` sits: the enum
#: groups it with the tree types (early), and the server puts it after every
#: leaf but ``$type``.
MATCH_TYPE_RANK: dict[str, int] = {
    "$and": 0,
    "$or": 1,
    "$nor": 2,
    "$elemMatch": 3,
    "$size": 5,
    "$eq": 6,
    "$lte": 7,
    "$lt": 8,
    "$gt": 9,
    "$gte": 10,
    "$regex": 11,
    "$mod": 12,
    "$exists": 13,
    "$in": 14,
    "$bitsAllSet": 15,
    "$bitsAllClear": 16,
    "$bitsAnySet": 17,
    "$bitsAnyClear": 18,
    "$not": 19,
    "$type": 20,
    "$expr": 21,
    "$_internalExprEq": 22,
}
#: Anything not in the table above sorts after everything that is, rather than
#: colliding at rank 0 with ``$and`` -- an unmeasured operator should not be
#: asserted to come first.
_UNRANKED = 99

#: ``$type``'s string aliases, as mongod renders them back: a numeric BSON type
#: code. ``"number"`` is the one alias with no single code, and mongod echoes
#: the STRING for it (probed 8.2.11) rather than expanding to the four numeric
#: ones.
_TYPE_ALIAS_CODES: dict[str, int] = {
    "double": 1,
    "string": 2,
    "object": 3,
    "array": 4,
    "binData": 5,
    "undefined": 6,
    "objectId": 7,
    "bool": 8,
    "date": 9,
    "null": 10,
    "regex": 11,
    "dbPointer": 12,
    "javascript": 13,
    "symbol": 14,
    "javascriptWithScope": 15,
    "int": 16,
    "timestamp": 17,
    "long": 18,
    "decimal": 19,
    "minKey": -1,
    "maxKey": 127,
}

#: Query-language keys that are neither a field path nor a clause: mongod drops
#: them from the parsed tree entirely.
_DROPPED_TOP_LEVEL = {"$comment"}

#: The bitwise operators. Each accepts a numeric MASK, a bit-POSITION array, or
#: BinData -- and echoes all three back as a bit-position array, so
#: ``$bitsAllSet: 1`` parses as ``$bitsAllSet: [0]`` (probed 8.2.11).
_BITS_OPS = {"$bitsAllSet", "$bitsAllClear", "$bitsAnySet", "$bitsAnyClear"}


def _bit_positions(arg: Any) -> Any:
    """A bitwise operator's argument as mongod echoes it: set-bit positions."""
    if isinstance(arg, bool) or not isinstance(arg, int):
        # An explicit position list (or BinData) is already in the echoed form.
        return arg
    if arg < 0:
        return arg
    return [i for i in range(arg.bit_length()) if arg >> i & 1]


def _rank(clause: Mapping[str, Any]) -> tuple[int, str]:
    """Sort key for one ``$and`` child: (match-type ordinal, path)."""
    key = next(iter(clause), "")
    if key.startswith("$"):
        return (MATCH_TYPE_RANK.get(key, _UNRANKED), "")
    value = clause[key]
    op = next(iter(value), "") if isinstance(value, Mapping) else ""
    return (MATCH_TYPE_RANK.get(op, _UNRANKED), key)


def _type_codes(spec: Any) -> Any:
    """``$type``'s argument as mongod echoes it: a sorted list of BSON codes."""
    values = spec if isinstance(spec, (list, tuple)) else [spec]
    out: list[Any] = []
    for v in values:
        if isinstance(v, str):
            code = _TYPE_ALIAS_CODES.get(v)
            # ``"number"`` has no single code; mongod keeps the alias.
            out.append(v if code is None else code)
        else:
            out.append(v)
    numeric = [v for v in out if not isinstance(v, str)]
    strings = [v for v in out if isinstance(v, str)]
    if strings and numeric:
        # Not measured (mongod may interleave); pass the input order through
        # rather than assert an order nobody probed.
        return out
    if strings:
        return strings
    return sorted(numeric)


def _field_clauses(path: str, value: Any) -> list[dict[str, Any]]:
    """The clause list one ``field: <value>`` entry expands to."""
    if not isinstance(value, Mapping) or not any(
        isinstance(k, str) and k.startswith("$") for k in value
    ):
        # A bare value -- including a whole sub-document, which is an equality
        # against that document and NOT a nested query.
        return [{path: {"$eq": value}}]

    out: list[dict[str, Any]] = []
    items = list(value.items())
    i = 0
    while i < len(items):
        op, arg = items[i]
        i += 1
        if op == "$ne":
            out.append({path: {"$not": {"$eq": arg}}})
        elif op == "$nin":
            out.append({path: {"$not": {"$in": arg}}})
        elif op == "$in":
            if isinstance(arg, (list, tuple)) and len(arg) == 0:
                out.append({"$alwaysFalse": 1})
            elif isinstance(arg, (list, tuple)) and len(arg) == 1:
                out.append({path: {"$eq": arg[0]}})
            else:
                out.append({path: {"$in": arg}})
        elif op == "$all" and isinstance(arg, (list, tuple)):
            # ``$all`` is AND-of-equalities, and the ``$elemMatch`` form is
            # AND-of-elemMatches. An empty ``$all`` matches nothing.
            if not arg:
                out.append({"$alwaysFalse": 1})
            for member in arg:
                if isinstance(member, Mapping) and set(member) == {"$elemMatch"}:
                    out.append({path: {"$elemMatch": canonical_match(member["$elemMatch"])}})
                else:
                    out.append({path: {"$eq": member}})
        elif op == "$elemMatch":
            inner = arg if isinstance(arg, Mapping) else {}
            if any(isinstance(k, str) and k.startswith("$") for k in inner):
                # The VALUE form (`{$elemMatch: {$gt: 1}}`) applies operators to
                # the elements themselves, so it is not a sub-document query.
                out.append({path: {"$elemMatch": arg}})
            else:
                out.append({path: {"$elemMatch": canonical_match(inner)}})
        elif op == "$type":
            out.append({path: {"$type": _type_codes(arg)}})
        elif op == "$regex":
            # ``$options`` belongs to the same clause as the ``$regex`` it
            # modifies -- mongod keeps the pair together.
            clause: dict[str, Any] = {"$regex": arg}
            if "$options" in value:
                clause["$options"] = value["$options"]
            out.append({path: clause})
        elif op == "$options":
            continue  # consumed by the $regex arm above
        elif op in _BITS_OPS:
            out.append({path: {op: _bit_positions(arg)}})
        else:
            out.append({path: {op: arg}})
    return out


def _clauses(filter_: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Flatten a filter document into mongod's list of ``$and`` children."""
    out: list[dict[str, Any]] = []
    for key, value in filter_.items():
        if key in _DROPPED_TOP_LEVEL:
            continue
        if key == "$and":
            for sub in value if isinstance(value, (list, tuple)) else []:
                if isinstance(sub, Mapping):
                    out.extend(_clauses(sub))
            continue
        if key == "$nor":
            children = [canonical_match(s) for s in (value or []) if isinstance(s, Mapping)]
            if len(children) >= 2:
                # Kept whole while it is the query's only clause; the caller
                # decomposes it if anything else joins it in an ``$and``.
                out.append({"$nor": children})
            else:
                for child in children:
                    out.extend(_negate(child))
            continue
        if key == "$or":
            children = [canonical_match(s) for s in (value or []) if isinstance(s, Mapping)]
            if len(children) == 1:
                out.extend(_clauses_of_canonical(children[0]))
            else:
                out.append({"$or": children})
            continue
        if key.startswith("$"):
            out.append({key: value})
            continue
        out.extend(_field_clauses(key, value))
    return out


def _negate(clause: Mapping[str, Any]) -> list[dict[str, Any]]:
    """``$nor``'s per-child negation: ``{a: {$eq: 1}}`` -> ``{a: {$not: {$eq: 1}}}``."""
    out: list[dict[str, Any]] = []
    for sub in _clauses_of_canonical(clause):
        path = next(iter(sub), "")
        if path.startswith("$"):
            out.append({"$nor": [sub]})
        else:
            out.append({path: {"$not": sub[path]}})
    return out


def _clauses_of_canonical(doc: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Re-open an already-canonical document into its clause list."""
    if len(doc) == 1 and "$and" in doc:
        return list(doc["$and"])
    return [dict(doc)] if doc else []


def canonical_match(filter_: Any) -> dict[str, Any]:
    """mongod's normalised form of ``filter_``, as ``parsedQuery`` reports it.

    Passes anything unmeasured through unchanged rather than guessing. The
    known residue against mongod 8.2.11 is ``$expr`` (which mongod splits into
    an ``$expr`` clause plus an ``$_internalExprEq`` index-usable twin) and
    ``$jsonSchema``; both are echoed as sent.
    """
    if not isinstance(filter_, Mapping) or not filter_:
        return {}
    clauses = _clauses(filter_)
    if any(len(c) == 1 and "$alwaysFalse" in c for c in clauses):
        return {"$alwaysFalse": 1}
    if not clauses:
        return {}
    if len(clauses) == 1:
        return clauses[0]
    # A ``$nor`` survives as a node only when it is the whole query. As soon as
    # it shares an ``$and`` with anything else, mongod merges it in as one
    # ``$not`` per child rather than nesting it (probed 8.2.11).
    expanded: list[dict[str, Any]] = []
    for clause in clauses:
        if len(clause) == 1 and "$nor" in clause:
            for child in clause["$nor"]:
                expanded.extend(_negate(child))
        else:
            expanded.append(clause)
    return {"$and": sorted(expanded, key=_rank)}


def projection_stage_name(projection: Mapping[str, Any]) -> str:
    """``PROJECTION_SIMPLE`` or ``PROJECTION_DEFAULT`` for this spec.

    mongod uses the fast path only for a flat inclusion / exclusion list; a
    dotted path or any operator (``$elemMatch`` / ``$slice`` / a computed
    expression) drops it to the general one. Probed 8.2.11.
    """
    for field, value in projection.items():
        if "." in field or isinstance(value, (Mapping, list, tuple)):
            return "PROJECTION_DEFAULT"
    return "PROJECTION_SIMPLE"


def build_stage_tree(
    base: dict[str, Any],
    *,
    sort: Mapping[str, Any] | None,
    sort_served_by_index: bool,
    projection: Mapping[str, Any] | None,
    skip: int | None,
    limit: int | None,
) -> dict[str, Any]:
    """Wrap the scan node ``base`` in mongod's query-shape stages.

    The nesting is mongod's, measured on 8.2.11 -- it is not the order the
    command's fields are written in:

    * a sort the index cannot serve becomes a blocking ``SORT`` directly above
      the scan, and it ABSORBS the limit (as ``limitAmount``, counting the
      skipped documents), so no separate ``LIMIT`` stage appears;
    * ``SKIP`` sits above the scan (or above the ``SORT``);
    * the projection sits above the skip;
    * a ``LIMIT`` not absorbed by a sort is the OUTERMOST stage, above the
      projection.
    """
    node = base
    skip_n = int(skip) if isinstance(skip, (int, float)) and skip and skip > 0 else 0
    # A negative ``limit`` is pymongo's "single batch" flag, not a smaller
    # limit; mongod reports its magnitude.
    limit_n = int(abs(limit)) if isinstance(limit, (int, float)) and limit else 0

    blocking_sort = bool(sort) and not sort_served_by_index
    absorbed = False
    if blocking_sort:
        sort_stage: dict[str, Any] = {
            "stage": "SORT",
            "sortPattern": dict(sort or {}),
            "memLimit": SORT_MEM_LIMIT_BYTES,
        }
        if limit_n:
            # The sort must retain everything the skip will later discard.
            sort_stage["limitAmount"] = limit_n + skip_n
            absorbed = True
        sort_stage["type"] = "simple"
        sort_stage["inputStage"] = node
        node = sort_stage
    if skip_n:
        node = {"stage": "SKIP", "skipAmount": skip_n, "inputStage": node}
    if projection:
        node = {
            "stage": projection_stage_name(projection),
            "transformBy": dict(projection),
            "inputStage": node,
        }
    if limit_n and not absorbed:
        node = {"stage": "LIMIT", "limitAmount": limit_n, "inputStage": node}
    return node
