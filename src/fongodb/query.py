from __future__ import annotations

from collections.abc import Callable, Mapping
from typing import Any


class _Missing:
    _instance: _Missing | None = None

    def __new__(cls) -> _Missing:
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __repr__(self) -> str:
        return "MISSING"


MISSING: Any = _Missing()


class QueryError(Exception):
    pass


def matches(doc: Mapping[str, Any], query: Mapping[str, Any]) -> bool:
    if not query:
        return True
    return all(_match_clause(doc, k, v) for k, v in query.items())


def _match_clause(doc: Mapping[str, Any], key: str, condition: Any) -> bool:
    if key == "$and":
        return all(matches(doc, c) for c in condition)
    if key == "$or":
        return any(matches(doc, c) for c in condition)
    if key == "$nor":
        return not any(matches(doc, c) for c in condition)
    if key.startswith("$"):
        raise QueryError(f"unsupported top-level operator: {key}")
    return _field_matches(_resolve_path(doc, key), condition)


def _resolve_path(doc: Any, path: str) -> list[Any]:
    parts = path.split(".")
    current: list[Any] = [doc]
    for part in parts:
        nxt: list[Any] = []
        for cur in current:
            if isinstance(cur, Mapping):
                nxt.append(cur.get(part, MISSING))
            elif isinstance(cur, list):
                if part.isdigit():
                    idx = int(part)
                    nxt.append(cur[idx] if 0 <= idx < len(cur) else MISSING)
                else:
                    for elem in cur:
                        if isinstance(elem, Mapping):
                            nxt.append(elem.get(part, MISSING))
            else:
                nxt.append(MISSING)
        current = nxt
    return current


def _field_matches(values: list[Any], condition: Any) -> bool:
    if isinstance(condition, Mapping) and condition and all(k.startswith("$") for k in condition):
        return all(_op_matches(values, op, arg) for op, arg in condition.items())
    return _eq_with_array(values, condition)


def _eq_with_array(values: list[Any], expected: Any) -> bool:
    for v in values:
        if v is MISSING:
            if expected is None:
                return True
            continue
        if v == expected:
            return True
        if isinstance(v, list) and any(e == expected for e in v):
            return True
    return False


def _op_matches(values: list[Any], op: str, arg: Any) -> bool:
    if op == "$eq":
        return _eq_with_array(values, arg)
    if op == "$ne":
        return not _eq_with_array(values, arg)
    if op == "$gt":
        return _cmp(values, arg, lambda a, b: a > b)
    if op == "$gte":
        return _cmp(values, arg, lambda a, b: a >= b)
    if op == "$lt":
        return _cmp(values, arg, lambda a, b: a < b)
    if op == "$lte":
        return _cmp(values, arg, lambda a, b: a <= b)
    if op == "$in":
        return any(_eq_with_array(values, candidate) for candidate in arg)
    if op == "$nin":
        return not any(_eq_with_array(values, candidate) for candidate in arg)
    if op == "$exists":
        present = any(v is not MISSING for v in values)
        return present == bool(arg)
    if op == "$not":
        return not _field_matches(values, arg)
    raise QueryError(f"unsupported query operator: {op}")


def _cmp(values: list[Any], target: Any, op: Callable[[Any, Any], bool]) -> bool:
    for v in values:
        if v is MISSING:
            continue
        if _try_cmp(v, target, op):
            return True
        if isinstance(v, list):
            for elem in v:
                if _try_cmp(elem, target, op):
                    return True
    return False


def _try_cmp(a: Any, b: Any, op: Callable[[Any, Any], bool]) -> bool:
    try:
        return bool(op(a, b))
    except TypeError:
        return False
