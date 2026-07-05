"""Postgres ``uuid`` type.

A UUID is stored as its canonical lower-case hyphenated string (e.g.
``550e8400-e29b-41d4-a716-446655440000``) — the same text Postgres renders — so
equality and ordering are plain string comparisons that lower to a Mongo filter
(no per-row evaluation needed). ``secantus.sql`` ``scalar`` / ``typemap`` /
``planner`` wire it into the SQL surface.

The module is named ``uuidtype`` (not ``uuid``) so it does not shadow the
standard-library ``uuid`` module it builds on.
"""

from __future__ import annotations

import uuid as _uuid
from typing import Any


class UUIDError(ValueError):
    """A malformed UUID literal."""


def normalize(text: Any) -> str:
    """Canonicalise a UUID literal to the lower-case hyphenated form. Accepts the
    hyphenated form, a bare 32-hex string, or a ``{...}``-braced form (as Postgres
    does)."""
    try:
        return str(_uuid.UUID(str(text).strip()))
    except (ValueError, AttributeError, TypeError) as e:
        raise UUIDError(f"invalid uuid value: {text!r}") from e


def generate() -> str:
    """A fresh random (v4) UUID as its canonical string — ``gen_random_uuid()`` /
    ``uuid_generate_v4()``."""
    return str(_uuid.uuid4())


def is_uuid_value(v: Any) -> bool:
    """Whether ``v`` is a string that parses as a UUID."""
    if not isinstance(v, str):
        return False
    try:
        _uuid.UUID(v)
    except ValueError:
        return False
    return True
