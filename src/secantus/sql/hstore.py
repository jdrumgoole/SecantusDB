"""Postgres ``hstore`` key/value type (the contrib extension): a flat string →
string map with an optional NULL value per key.

Stored as a **tagged** subdocument ``{"hstore": {k: v_or_None, …}}`` so it is
distinguishable from a plain ``jsonb`` object (both are dicts) — the ``@>`` /
``<@`` / ``?`` / ``?&`` / ``?|`` / ``->`` operators are shared with jsonb, so the
scalar evaluator disambiguates on this tag and hstore predicates route through
the per-row path rather than jsonb's Mongo lowering.

Text form (Postgres canonical output): ``"a"=>"1", "b"=>"2"`` — keys and non-NULL
values double-quoted, a NULL value rendered as a bare ``NULL``. Input is lenient:
keys / values may be unquoted when they contain no whitespace, comma, or ``=>``.

Out of scope: the set-returning ``each`` / ``skeys`` / ``svals`` forms (the array
functions ``akeys`` / ``avals`` cover the common need), GiST/GIN indexing, and the
``#=`` / ``%%`` / ``%#`` record operators.
"""

from __future__ import annotations

from typing import Any

_TAG = "hstore"


class HstoreError(ValueError):
    """A malformed ``hstore`` literal."""


def _tag(d: dict[str, Any]) -> dict[str, Any]:
    return {_TAG: d}


def is_hstore(v: Any) -> bool:
    """Whether ``v`` is a stored hstore (the tagged subdocument form)."""
    return isinstance(v, dict) and len(v) == 1 and _TAG in v and isinstance(v[_TAG], dict)


def as_map(v: Any) -> dict[str, Any]:
    """The plain ``{key: value}`` map inside a tagged hstore (or ``v`` itself if it
    is already a plain dict)."""
    if is_hstore(v):
        return v[_TAG]
    if isinstance(v, dict):
        return v
    raise HstoreError(f"not an hstore value: {v!r}")


def _skip_ws(s: str, i: int) -> int:
    while i < len(s) and s[i] in " \t\n\r":
        i += 1
    return i


def _read_token(s: str, i: int) -> tuple[Any, int]:
    """Read one hstore token (a quoted string, a bare word, or bare NULL) starting
    at ``i``. Returns ``(value, next_index)``; a bare ``NULL`` reads as ``None``."""
    i = _skip_ws(s, i)
    if i >= len(s):
        raise HstoreError("unexpected end of hstore literal")
    if s[i] == '"':
        i += 1
        buf: list[str] = []
        while i < len(s) and s[i] != '"':
            if s[i] == "\\" and i + 1 < len(s):
                buf.append(s[i + 1])
                i += 2
            else:
                buf.append(s[i])
                i += 1
        if i >= len(s):
            raise HstoreError("unterminated quoted string in hstore literal")
        return "".join(buf), i + 1  # skip closing quote
    # Bare token: read until whitespace, comma, or the '=>' separator.
    start = i
    while i < len(s) and s[i] not in " \t\n\r," and not s.startswith("=>", i):
        i += 1
    token = s[start:i]
    if token == "":
        raise HstoreError(f"empty token in hstore literal at offset {start}")
    return (None if token.upper() == "NULL" else token), i


def parse(value: Any) -> dict[str, Any]:
    """Parse an hstore text literal into the tagged subdocument form. A dict passes
    through (tagged); an already-tagged value is returned unchanged."""
    if is_hstore(value):
        return value
    if isinstance(value, dict):
        return _tag({str(k): (None if v is None else str(v)) for k, v in value.items()})
    s = str(value).strip()
    out: dict[str, Any] = {}
    i = 0
    while True:
        i = _skip_ws(s, i)
        if i >= len(s):
            break
        key, i = _read_token(s, i)
        if key is None:
            raise HstoreError("hstore key cannot be NULL")
        i = _skip_ws(s, i)
        if not s.startswith("=>", i):
            raise HstoreError(f"expected '=>' after key {key!r} in hstore literal")
        i += 2
        val, i = _read_token(s, i)
        out[str(key)] = val
        i = _skip_ws(s, i)
        if i < len(s) and s[i] == ",":
            i += 1
    return _tag(out)


def _quote(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def render(value: Any) -> str:
    """Render a (tagged or plain) hstore as Postgres canonical text."""
    m = as_map(value)
    parts = [f"{_quote(str(k))}=>{'NULL' if v is None else _quote(str(v))}" for k, v in m.items()]
    return ", ".join(parts)


# --------------------------------------------------------------------------- #
# Operators
# --------------------------------------------------------------------------- #


def contains(a: Any, b: Any) -> bool:
    """``a @> b`` — does ``a`` contain every key/value pair of ``b``?"""
    am, bm = as_map(a), as_map(parse(b) if not is_hstore(b) and isinstance(b, str) else b)
    return all(k in am and am[k] == v for k, v in bm.items())


def contained_by(a: Any, b: Any) -> bool:
    """``a <@ b`` — is every pair of ``a`` present in ``b``?"""
    return contains(b, a)


def exists(a: Any, key: Any) -> bool:
    """``a ? key`` — is ``key`` present (regardless of value)?"""
    return str(key) in as_map(a)


def exists_all(a: Any, keys: Any) -> bool:
    """``a ?& keys`` — are all of ``keys`` present?"""
    m = as_map(a)
    return all(str(k) in m for k in keys)


def exists_any(a: Any, keys: Any) -> bool:
    """``a ?| keys`` — is any of ``keys`` present?"""
    m = as_map(a)
    return any(str(k) in m for k in keys)


def lookup(a: Any, key: Any) -> Any:
    """``a -> key`` — the value for ``key`` (``None`` if absent or NULL)."""
    return as_map(a).get(str(key))


def merge(a: Any, b: Any) -> dict[str, Any]:
    """``a || b`` — merge, with ``b``'s pairs overriding ``a``'s."""
    out = dict(as_map(a))
    out.update(as_map(b))
    return _tag(out)


def delete(a: Any, key: Any) -> dict[str, Any]:
    """``delete(hstore, key)`` — a copy without ``key``."""
    out = {k: v for k, v in as_map(a).items() if k != str(key)}
    return _tag(out)


def defined(a: Any, key: Any) -> bool:
    """``defined(hstore, key)`` — is ``key`` present *and* its value non-NULL?"""
    m = as_map(a)
    return str(key) in m and m[str(key)] is not None


def akeys(a: Any) -> list[str]:
    """``akeys(hstore)`` — the keys as a text array."""
    return list(as_map(a).keys())


def avals(a: Any) -> list[Any]:
    """``avals(hstore)`` — the values as a text array (NULLs preserved)."""
    return list(as_map(a).values())


def to_json(a: Any) -> dict[str, Any]:
    """``hstore_to_json(hstore)`` — a plain JSON object (untagged)."""
    return dict(as_map(a))


def from_pair(key: Any, value: Any) -> dict[str, Any]:
    """``hstore(key, value)`` — a single-pair hstore."""
    return _tag({str(key): (None if value is None else str(value))})


def from_arrays(keys: Any, values: Any) -> dict[str, Any]:
    """``hstore(text[], text[])`` — pair up keys with values."""
    return _tag(
        {str(k): (None if v is None else str(v)) for k, v in zip(keys, values, strict=False)}
    )
